//! SFTP v3 request handling, rooted at the shared workspace directory.
//!
//! The service answers the packet set that `sshfs` uses, plus the extensions the product depends
//! on (`posix-rename@openssh.com` and `fsync@openssh.com`). Everything is resolved through
//! [`ShareRoot`], so a request cannot address anything outside the shared root, and every file
//! attribute is derived from a verified Windows file identity plus the mode metadata database.
//!
//! Notes that matter for correctness:
//!
//! * `open` honours the permission bits the client sends when it creates a file. That is the
//!   documented meaning of the `attrs` field of `SSH_FXP_OPEN`, and it is what makes the mode of
//!   a freshly linked executable match what the Linux toolchain asked for.
//! * `rename` follows the version-3 specification and fails when the destination exists, while
//!   `posix-rename@openssh.com` replaces it. Windows renaming is done with
//!   `MoveFileEx(..., MOVEFILE_REPLACE_EXISTING)` through `std::fs::rename`, so a replacement is a
//!   single file system operation. When another process holds the destination open, the failure
//!   is reported with an explicit message instead of silently losing data.
//! * Reads and writes use positioned I/O, so concurrent readers of the same handle do not disturb
//!   each other.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, FileMode, Handle, Name, OpenFlags, Packet, Status,
    StatusCode, Version,
};
use russh_sftp::server::{run_with_config, Config, Handler, StatusReply};

use crate::error::OiError;
use crate::share::handle::{
    apply_chmod, compose_mode, identity_of_handle, identity_of_path, read_dir_entries,
    EntryInfo,
};
use crate::share::mode_db::{FileIdentity, FileKind, ModeDb, SIMULATED_GID, SIMULATED_UID};
use crate::share::path_guard::{self, ShareRoot};
use crate::timeutil::now_unix;

pub const EXT_POSIX_RENAME: &str = "posix-rename@openssh.com";
pub const EXT_FSYNC: &str = "fsync@openssh.com";

/// Serve one SFTP channel to completion.
pub async fn run_sftp<S>(
    stream: S,
    root: ShareRoot,
    db: Arc<ModeDb>,
    guest_workspace: String,
) -> Result<(), OiError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let handler = SftpHandler::new(root, db, guest_workspace);
    run_with_config(stream, handler, Config::default()).await;
    Ok(())
}

struct OpenFile {
    path: PathBuf,
    file: std::fs::File,
    identity: FileIdentity,
    append: bool,
    writable: bool,
}

struct OpenDir {
    path: PathBuf,
    entries: Vec<File>,
    served: usize,
}

/// Approximate size a directory entry adds to a `SSH_FXP_NAME` packet: the file name, the 32-byte
/// long name (equal to the short one here) and the attribute block.
const ENTRY_OVERHEAD: usize = 160;

/// One `SSH_FXP_NAME` packet must stay well below the SSH channel's maximum packet size. OpenSSH's
/// sftp-server sends entries in batches for the same reason; a single huge packet is rejected (or
/// silently dropped) by real clients, which then see an empty directory.
const READDIR_BATCH_BYTES: usize = 16 * 1024;

enum HandleEntry {
    File(OpenFile),
    Dir(OpenDir),
}

pub struct SftpHandler {
    root: ShareRoot,
    db: Arc<ModeDb>,
    guest_workspace: String,
    handles: HashMap<String, HandleEntry>,
    next_handle: u64,
}

impl SftpHandler {
    pub fn new(root: ShareRoot, db: Arc<ModeDb>, guest_workspace: String) -> Self {
        SftpHandler {
            root,
            db,
            guest_workspace,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    fn fail(err: OiError) -> StatusReply {
        match &err {
            OiError::PathEscapesRoot(_) | OiError::PathNotAllowed(_) => {
                StatusReply::new(StatusCode::PermissionDenied).with_message(err.to_string())
            }
            OiError::FileLocked(_) => StatusReply::new(StatusCode::Failure)
                .with_message(format!("{err} (the file is in use by another Windows process)")),
            OiError::ShareUnavailable(_) | OiError::IdentityUnverifiable(_) => {
                StatusReply::new(StatusCode::Failure).with_message(err.to_string())
            }
            OiError::Io { source, path } => match source.kind() {
                std::io::ErrorKind::NotFound => StatusReply::new(StatusCode::NoSuchFile)
                    .with_message(format!("{} does not exist", path.display())),
                std::io::ErrorKind::PermissionDenied => {
                    StatusReply::new(StatusCode::PermissionDenied).with_message(err.to_string())
                }
                _ => StatusReply::new(StatusCode::Failure).with_message(err.to_string()),
            },
            _ => StatusReply::new(StatusCode::Failure).with_message(err.to_string()),
        }
    }

    fn ok() -> Status {
        Status {
            id: 0,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, StatusReply> {
        // `sshfs` speaks paths relative to the SFTP root, but a client may also use the absolute
        // path it sees in the guest (`/home/student/workspace/...`). Both mean the same file, so
        // the mount prefix is stripped before validation instead of being treated as a directory
        // name inside the workspace.
        let normalised = match path.strip_prefix(self.guest_workspace.as_str()) {
            Some(rest) if rest.is_empty() => "/".to_string(),
            Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => rest.to_string(),
            _ => path.to_string(),
        };
        let resolved = self.root.resolve(&normalised).map_err(Self::fail)?;
        self.root
            .reject_reparse_in_existing_ancestors(&resolved)
            .map_err(Self::fail)?;
        Ok(resolved)
    }

    fn attributes(&self, path: &PathBuf, follow: bool) -> Result<FileAttributes, StatusReply> {
        let meta = if follow {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        }
        .map_err(|e| Self::fail(OiError::io(path, e)))?;

        let kind = if meta.is_dir() {
            FileKind::Dir
        } else {
            FileKind::File
        };
        let identity = identity_of_path(path, kind).map_err(Self::fail)?;
        let stored = self
            .db
            .get_or_default(&identity, kind)
            .map_err(Self::fail)?;
        let mode = compose_mode(stored, kind, false);
        Ok(attributes_from(&meta, kind, mode))
    }

    /// Directory listing with a single metadata query for the whole directory, so that listing a
    /// large workspace does not turn into one database round trip per entry.
    fn listing(&self, path: &PathBuf) -> Result<Vec<File>, StatusReply> {
        let entries: Vec<EntryInfo> = read_dir_entries(path).map_err(Self::fail)?;
        let mut modes: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut volume: Option<String> = None;
        for e in &entries {
            if let Some(id) = &e.identity {
                volume = Some(id.volume_id.clone());
                break;
            }
        }
        if let Some(volume) = volume {
            modes = self
                .db
                .snapshot_volume(&volume)
                .map_err(Self::fail)?;
        }
        let mut files = Vec::with_capacity(entries.len());
        for e in entries {
            let meta = std::fs::symlink_metadata(&e.path).map_err(|err| Self::fail(OiError::io(&e.path, err)))?;
            let stored = e
                .identity
                .as_ref()
                .and_then(|id| modes.get(&id.file_id).copied())
                .unwrap_or(0);
            let mode = compose_mode(stored, e.kind, e.is_reparse_point);
            files.push(File::new(
                e.name.clone(),
                attributes_from(&meta, e.kind, mode),
            ));
        }
        Ok(files)
    }

    fn new_handle(&mut self, entry: HandleEntry) -> String {
        let id = format!("oi{:016x}", self.next_handle);
        self.next_handle += 1;
        self.handles.insert(id.clone(), entry);
        id
    }

    fn file_mut(&mut self, handle: &str) -> Result<&mut OpenFile, StatusReply> {
        match self.handles.get_mut(handle) {
            Some(HandleEntry::File(f)) => Ok(f),
            Some(_) => Err(StatusReply::new(StatusCode::Failure)
                .with_message("the handle refers to a directory")),
            None => Err(StatusReply::new(StatusCode::Failure)
                .with_message("unknown file handle")),
        }
    }

    fn dir_mut(&mut self, handle: &str) -> Result<&mut OpenDir, StatusReply> {
        match self.handles.get_mut(handle) {
            Some(HandleEntry::Dir(d)) => Ok(d),
            Some(_) => Err(StatusReply::new(StatusCode::Failure)
                .with_message("the handle refers to a file")),
            None => Err(StatusReply::new(StatusCode::Failure)
                .with_message("unknown directory handle")),
        }
    }

    /// Persist the mode the client asked for, if it asked for one.
    fn apply_permissions(
        &self,
        identity: &FileIdentity,
        kind: FileKind,
        attrs: &FileAttributes,
    ) -> Result<bool, StatusReply> {
        let Some(requested) = attrs.permissions else {
            return Ok(false);
        };
        let current = self
            .db
            .get_or_default(identity, kind)
            .map_err(Self::fail)?;
        let next = apply_chmod(current, requested, kind);
        if next != current || !self.db.has_record(identity).map_err(Self::fail)? {
            self.db.set_mode(identity, kind, next).map_err(Self::fail)?;
        }
        Ok(true)
    }

    fn extended_posix_rename(&mut self, old: &str, new: &str) -> Result<Status, StatusReply> {
        let from = self.resolve(old)?;
        let to = self.resolve(new)?;
        if !from.exists() {
            return Err(Self::fail(OiError::io(
                &from,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            )));
        }
        // Reject replacing anything outside the root, including through a reparse point.
        self.root.verify_contained(&to).map_err(Self::fail)?;
        match std::fs::rename(&from, &to) {
            Ok(()) => Ok(Self::ok()),
            Err(e) => Err(Self::fail(OiError::io(&to, e))),
        }
    }
}

fn attributes_from(meta: &std::fs::Metadata, kind: FileKind, mode: u32) -> FileAttributes {
    let type_bits = match kind {
        FileKind::Dir => FileMode::DIR.bits(),
        FileKind::File => FileMode::REG.bits(),
    };
    FileAttributes {
        size: Some(meta.len()),
        uid: Some(SIMULATED_UID),
        user: Some(crate::GUEST_USER.to_string()),
        gid: Some(SIMULATED_GID),
        group: Some(crate::GUEST_USER.to_string()),
        permissions: Some(mode & 0o7777 | type_bits),
        atime: Some(crate::share::handle::filetime_to_unix(
            crate::share::handle::filetime_of(meta),
        )),
        mtime: Some(crate::share::handle::filetime_to_unix(
            crate::share::handle::filetime_of(meta),
        )),
    }
}

impl Handler for SftpHandler {
    type Error = StatusReply;

    fn unimplemented(&self) -> StatusReply {
        StatusReply::new(StatusCode::OpUnsupported)
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, StatusReply> {
        let mut version = Version::new();
        version
            .extensions
            .insert(EXT_POSIX_RENAME.to_string(), "1".to_string());
        version
            .extensions
            .insert(EXT_FSYNC.to_string(), "1".to_string());
        Ok(version)
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusReply> {
        let resolved = self.resolve(&path)?;
        let sftp = path_guard::to_sftp_path(&self.root.configured(), &resolved);
        Ok(Name {
            id,
            files: vec![File::dummy(sftp)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        let resolved = self.resolve(&path)?;
        let attrs = self.attributes(&resolved, true)?;
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        let resolved = self.resolve(&path)?;
        let attrs = self.attributes(&resolved, false)?;
        Ok(Attrs { id, attrs })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusReply> {
        match self.handles.get(&handle) {
            Some(HandleEntry::File(f)) => {
                let meta = f
                    .file
                    .metadata()
                    .map_err(|e| Self::fail(OiError::io(&f.path, e)))?;
                let stored = self
                    .db
                    .get_or_default(&f.identity, FileKind::File)
                    .map_err(Self::fail)?;
                let mode = compose_mode(stored, FileKind::File, false);
                Ok(Attrs {
                    id,
                    attrs: attributes_from(&meta, FileKind::File, mode),
                })
            }
            Some(HandleEntry::Dir(d)) => {
                let attrs = self.attributes(&d.path.clone(), false)?;
                Ok(Attrs { id, attrs })
            }
            None => Err(StatusReply::new(StatusCode::Failure)
                .with_message("unknown file handle")),
        }
    }

    async fn open(
        &mut self,
        _id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, StatusReply> {
        let path = self.resolve(&filename)?;
        if path.is_dir() {
            return Err(StatusReply::new(StatusCode::Failure)
                .with_message("the target is a directory"));
        }
        let creating = pflags.contains(OpenFlags::CREATE);
        let existed = path.exists();

        let mut opts = std::fs::OpenOptions::new();
        let read = pflags.contains(OpenFlags::READ);
        let write = pflags.contains(OpenFlags::WRITE);
        opts.read(read || !write);
        if write {
            opts.write(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            opts.append(true);
        }
        if creating {
            if pflags.contains(OpenFlags::EXCLUDE) {
                opts.create_new(true);
            } else {
                opts.create(true);
            }
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            opts.truncate(true);
        }

        let file = path_guard::open_no_follow(&mut opts, &path).map_err(Self::fail)?;
        // The handle is open; confirm that its parent chain really is inside the root.
        self.root.verify_contained(&path).map_err(Self::fail)?;

        let identity = identity_of_handle(&file).map_err(Self::fail)?;
        if !existed && creating {
            self.apply_permissions(&identity, FileKind::File, &attrs)?;
        }

        let handle = self.new_handle(HandleEntry::File(OpenFile {
            path,
            file,
            identity,
            append: pflags.contains(OpenFlags::APPEND),
            writable: write || pflags.contains(OpenFlags::APPEND),
        }));
        Ok(Handle {
            id: _id,
            handle,
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusReply> {
        match self.handles.remove(&handle) {
            Some(HandleEntry::File(f)) => {
                // Make sure the bytes are on the NTFS volume before the guest believes the write
                // completed; this is what keeps "Linux wrote it" and "Windows sees it" aligned.
                // Only handles that were opened for writing need this (and only they can do it).
                if f.writable {
                    if let Err(e) = f.file.sync_all() {
                        tracing::warn!(error = %e, path = %f.path.display(), "sync on close failed");
                    }
                }
                Ok(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: "Ok".into(),
                    language_tag: "en-US".into(),
                })
            }
            Some(HandleEntry::Dir(_)) => Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".into(),
                language_tag: "en-US".into(),
            }),
            None => Err(StatusReply::new(StatusCode::Failure)
                .with_message("unknown file handle")),
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusReply> {
        let f = self.file_mut(&handle)?;
        let mut buf = vec![0u8; len as usize];
        let mut file = &f.file;
        use std::io::Read;
        let read = if f.append {
            // append-only handles are positioned at the end by the writer
            match file.read(&mut buf) {
                Ok(n) => n,
                Err(e) => return Err(Self::fail(OiError::io(&f.path, e))),
            }
        } else {
            match positioned_read(&mut file, &mut buf, offset) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
                Err(e) => return Err(Self::fail(OiError::io(&f.path, e))),
            }
        };
        if read == 0 {
            return Err(StatusReply::new(StatusCode::Eof));
        }
        buf.truncate(read);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusReply> {
        let f = self.file_mut(&handle)?;
        let path = f.path.clone();
        let result = if f.append {
            use std::io::Write;
            (&f.file).write_all(&data).and_then(|_| Ok(data.len()))
        } else {
            let mut file = &f.file;
            positioned_write(&mut file, &data, offset)
        };
        match result {
            Ok(n) if n == data.len() => Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".into(),
                language_tag: "en-US".into(),
            }),
            Ok(n) => Err(StatusReply::new(StatusCode::Failure)
                .with_message(format!("only {n} of {} bytes were written", data.len()))),
            Err(e) => Err(Self::fail(OiError::io(&path, e))),
        }
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let resolved = self.resolve(&path)?;
        let kind = if resolved.is_dir() {
            FileKind::Dir
        } else {
            FileKind::File
        };
        let identity = identity_of_path(&resolved, kind).map_err(Self::fail)?;
        self.apply_permissions(&identity, kind, &attrs)?;
        apply_size_and_time(&resolved, &attrs).map_err(Self::fail)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let (path, identity) = match self.handles.get(&handle) {
            Some(HandleEntry::File(f)) => (f.path.clone(), f.identity.clone()),
            Some(HandleEntry::Dir(d)) => {
                let p = d.path.clone();
                let identity = identity_of_path(&p, FileKind::Dir).map_err(Self::fail)?;
                (p, identity)
            }
            None => {
                return Err(StatusReply::new(StatusCode::Failure)
                    .with_message("unknown file handle"))
            }
        };
        self.apply_permissions(&identity, FileKind::File, &attrs)?;
        apply_size_and_time(&path, &attrs).map_err(Self::fail)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn opendir(&mut self, _id: u32, path: String) -> Result<Handle, StatusReply> {
        let resolved = self.resolve(&path)?;
        if !resolved.is_dir() {
            return Err(Self::fail(OiError::io(
                &resolved,
                std::io::Error::new(std::io::ErrorKind::NotFound, "not a directory"),
            )));
        }
        let entries = self.listing(&resolved)?;
        let handle = self.new_handle(HandleEntry::Dir(OpenDir {
            path: resolved,
            entries,
            served: 0,
        }));
        Ok(Handle {
            id: _id,
            handle,
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusReply> {
        let dir = self.dir_mut(&handle)?;
        if dir.served >= dir.entries.len() {
            return Err(StatusReply::new(StatusCode::Eof));
        }
        // Send a bounded batch and remember how far the listing has been served: clients keep
        // asking until EOF, and a directory with thousands of entries must not become one giant
        // packet.
        let mut files = Vec::new();
        let mut bytes = 0usize;
        while dir.served < dir.entries.len() {
            let entry = &dir.entries[dir.served];
            let size = entry.filename.len() * 2 + ENTRY_OVERHEAD;
            if !files.is_empty() && bytes + size > READDIR_BATCH_BYTES {
                break;
            }
            bytes += size;
            files.push(entry.clone());
            dir.served += 1;
        }
        Ok(Name { id, files })
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let resolved = self.resolve(&path)?;
        if resolved.exists() {
            return Err(Self::fail(OiError::io(
                &resolved,
                std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already exists"),
            )));
        }
        std::fs::create_dir(&resolved).map_err(|e| Self::fail(OiError::io(&resolved, e)))?;
        let identity = identity_of_path(&resolved, FileKind::Dir).map_err(Self::fail)?;
        let requested = attrs
            .permissions
            .map(|p| apply_chmod(0, p, FileKind::Dir))
            .unwrap_or_else(|| FileKind::Dir.default_mode(self.db.umask()));
        self.db
            .set_mode(&identity, FileKind::Dir, requested)
            .map_err(Self::fail)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusReply> {
        let resolved = self.resolve(&path)?;
        if !resolved.is_dir() {
            return Err(StatusReply::new(StatusCode::Failure)
                .with_message("not a directory"));
        }
        let identity = identity_of_path(&resolved, FileKind::Dir).map_err(Self::fail)?;
        std::fs::remove_dir(&resolved).map_err(|e| Self::fail(OiError::io(&resolved, e)))?;
        let _ = self.db.forget(&identity);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusReply> {
        let resolved = self.resolve(&filename)?;
        if resolved.is_dir() {
            return Err(StatusReply::new(StatusCode::Failure)
                .with_message("the target is a directory"));
        }
        let identity = identity_of_path(&resolved, FileKind::File).ok();
        std::fs::remove_file(&resolved).map_err(|e| Self::fail(OiError::io(&resolved, e)))?;
        if let Some(identity) = identity {
            let _ = self.db.forget(&identity);
        }
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusReply> {
        let from = self.resolve(&oldpath)?;
        let to = self.resolve(&newpath)?;
        if !from.exists() {
            return Err(Self::fail(OiError::io(
                &from,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            )));
        }
        if to.exists() {
            // Version 3 of the protocol requires this to be an error; clients that want a
            // replacing rename use posix-rename.
            return Err(StatusReply::new(StatusCode::Failure)
                .with_message("the destination already exists; posix-rename replaces it"));
        }
        std::fs::rename(&from, &to).map_err(|e| Self::fail(OiError::io(&to, e)))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn symlink(
        &mut self,
        _id: u32,
        _linkpath: String,
        _targetpath: String,
    ) -> Result<Status, StatusReply> {
        // The shared directory supports ordinary files and directories only.
        Err(StatusReply::new(StatusCode::OpUnsupported)
            .with_message("symbolic links are not supported in the shared workspace"))
    }

    async fn readlink(&mut self, _id: u32, path: String) -> Result<Name, StatusReply> {
        let resolved = self.resolve(&path)?;
        Err(Self::fail(OiError::PathNotAllowed(format!(
            "{} is not a symbolic link; links are not supported in the shared workspace",
            resolved.display()
        ))))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, StatusReply> {
        match request.as_str() {
            EXT_POSIX_RENAME => {
                let (old, new) = parse_two_strings(&data)?;
                let status = self.extended_posix_rename(&old, &new)?;
                Ok(Packet::Status(Status { id, ..status }))
            }
            EXT_FSYNC => {
                let path = parse_one_string(&data)?;
                let resolved = self.resolve(&path)?;
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .open(&resolved)
                    .map_err(|e| Self::fail(OiError::io(&resolved, e)))?;
                file.sync_all()
                    .map_err(|e| Self::fail(OiError::io(&resolved, e)))?;
                Ok(Packet::Status(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: "Ok".into(),
                    language_tag: "en-US".into(),
                }))
            }
            other => {
                tracing::debug!(extension = other, "refusing an unadvertised extension");
                Ok(Packet::Status(Status {
                    id,
                    status_code: StatusCode::OpUnsupported,
                    error_message: format!("{other} is not supported by this server"),
                    language_tag: "en-US".into(),
                }))
            }
        }
    }
}

fn apply_size_and_time(path: &PathBuf, attrs: &FileAttributes) -> Result<(), OiError> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(size) = attrs.size {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| OiError::io(path, e))?;
        file.set_len(size).map_err(|e| OiError::io(path, e))?;
    }
    if let Some(mtime) = attrs.mtime {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| OiError::io(path, e))?;
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime as u64);
        file.set_modified(t).map_err(|e| OiError::io(path, e))?;
    }
    let _ = now_unix();
    Ok(())
}

fn parse_one_string(data: &[u8]) -> Result<String, StatusReply> {
    let mut cursor = std::io::Cursor::new(data);
    read_string(&mut cursor)
}

fn parse_two_strings(data: &[u8]) -> Result<(String, String), StatusReply> {
    let mut cursor = std::io::Cursor::new(data);
    let a = read_string(&mut cursor)?;
    let b = read_string(&mut cursor)?;
    Ok((a, b))
}

fn read_string(cursor: &mut std::io::Cursor<&[u8]>) -> Result<String, StatusReply> {
    use std::io::Read;
    let mut len = [0u8; 4];
    cursor
        .read_exact(&mut len)
        .map_err(|_| StatusReply::new(StatusCode::BadMessage).with_message("truncated payload"))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 4096 {
        return Err(
            StatusReply::new(StatusCode::BadMessage).with_message("oversized string field")
        );
    }
    let mut buf = vec![0u8; len];
    cursor
        .read_exact(&mut buf)
        .map_err(|_| StatusReply::new(StatusCode::BadMessage).with_message("truncated payload"))?;
    String::from_utf8(buf)
        .map_err(|_| StatusReply::new(StatusCode::BadMessage).with_message("invalid UTF-8"))
}

#[cfg(windows)]
fn positioned_read(file: &mut &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

#[cfg(windows)]
fn positioned_write(file: &mut &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}

#[cfg(not(windows))]
fn positioned_read(file: &mut &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

#[cfg(not(windows))]
fn positioned_write(file: &mut &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

impl std::fmt::Debug for SftpHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpHandler")
            .field("root", &self.root.configured())
            .field("handles", &self.handles.len())
            .finish()
    }
}

/// Filesystem-facing helper used by the integration tests: the SFTP path of the guest workspace.
pub fn guest_workspace_path(guest_workspace: &str) -> String {
    guest_workspace.to_string()
}
