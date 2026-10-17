//! Windows file identity and directory enumeration.
//!
//! File identity comes from the file system through a handle (`volume serial`, `file index`,
//! `creation time`), never from the client-supplied path, so that metadata cannot be redirected
//! to a different object (`OILinux_PRD.md` §7.3). NTFS reuses file ids after deletion, which is
//! why the creation time is part of the key: it reduces, but does not eliminate, the chance of a
//! mistaken association.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::{OiError, OiResult};
use crate::share::mode_db::{FileIdentity, FileKind};

use super::path_guard;

#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_filetime: u64,
    pub is_reparse_point: bool,
    pub identity: Option<FileIdentity>,
}

/// Identity of an already opened handle.
pub fn identity_of_handle(file: &File) -> OiResult<FileIdentity> {
    #[cfg(windows)]
    {
        windows_identity(file)
    }
    #[cfg(not(windows))]
    {
        let meta = file
            .metadata()
            .map_err(|e| OiError::IdentityUnverifiable(format!("handle metadata failed: {e}")))?;
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity::new(
            format!("{:x}", meta.dev()),
            meta.ino().to_le_bytes().to_vec(),
            format!("{}", meta.ctime()),
        ))
    }
}

/// Volume serial, file index and creation time, all obtained from the file system through the
/// open handle. NTFS reuses file ids after deletion, which is why the creation time is part of
/// the key.
#[cfg(windows)]
fn windows_identity(file: &File) -> OiResult<FileIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info).map_err(|e| {
            OiError::IdentityUnverifiable(format!("GetFileInformationByHandle failed: {e}"))
        })?;
    }
    let file_index =
        ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    let created = ((info.ftCreationTime.dwHighDateTime as u64) << 32)
        | (info.ftCreationTime.dwLowDateTime as u64);
    Ok(FileIdentity::new(
        format!("{:08x}", info.dwVolumeSerialNumber),
        file_index.to_le_bytes().to_vec(),
        created.to_string(),
    ))
}

/// Identity of a path, obtained by opening it without following reparse points where possible.
pub fn identity_of_path(path: &Path, kind: FileKind) -> OiResult<FileIdentity> {
    match kind {
        FileKind::File => {
            let mut opts = std::fs::OpenOptions::new();
            opts.read(true);
            let f = path_guard::open_no_follow(&mut opts, path)?;
            identity_of_handle(&f)
        }
        FileKind::Dir => {
            let f = open_directory(path)?;
            identity_of_handle(&f)
        }
    }
}

/// Open a directory handle, which requires `FILE_FLAG_BACKUP_SEMANTICS` on Windows.
pub fn open_directory(path: &Path) -> OiResult<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        opts.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    opts.open(path).map_err(|e| OiError::io(path, e))
}

/// List a directory, attaching best-effort identity information to each entry.
pub fn read_dir_entries(dir: &Path) -> OiResult<Vec<EntryInfo>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| OiError::io(dir, e))? {
        let entry = entry.map_err(|e| OiError::io(dir, e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let sym_meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_reparse = path_guard::is_reparse_point(&path).unwrap_or(false);
        let is_dir = sym_meta.is_dir();
        let kind = if is_dir { FileKind::Dir } else { FileKind::File };
        let size = sym_meta.len();
        let mtime = filetime_of(&sym_meta);
        let identity = if is_reparse {
            None
        } else {
            identity_of_path(&path, kind).ok()
        };
        out.push(EntryInfo {
            name,
            path,
            kind,
            size,
            mtime_filetime: mtime,
            is_reparse_point: is_reparse,
            identity,
        });
    }
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    Ok(out)
}

pub fn filetime_of(meta: &std::fs::Metadata) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.last_write_time()
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.mtime() as u64) * 10_000_000 + 116_444_736_000_000_000
    }
}

/// Windows `FILETIME` (100 ns ticks since 1601) to unix seconds.
pub fn filetime_to_unix(filetime: u64) -> u32 {
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    let secs = filetime / 10_000_000;
    secs.saturating_sub(EPOCH_DIFF_SECS).min(u32::MAX as u64) as u32
}

#[cfg(windows)]
const ATTR_READONLY: u32 = 0x1;
#[cfg(windows)]
const ATTR_HIDDEN: u32 = 0x2;
#[cfg(windows)]
const ATTR_DIRECTORY: u32 = 0x10;

/// POSIX mode as the share presents it: the persisted execution bits combined with the
/// read/write bits Windows itself controls.
pub fn compose_mode(stored: u32, kind: FileKind, reparse: bool) -> u32 {
    let mut mode = if stored == 0 {
        kind.default_mode(crate::share::mode_db::DEFAULT_UMASK)
    } else {
        stored
    };
    // Windows read-only attribute maps onto the write bits.
    let executable = mode & 0o111 != 0;
    let mut base = if kind == FileKind::Dir {
        0o755
    } else {
        0o644
    };
    if executable {
        base |= 0o111;
    }
    if reparse {
        base |= 0o777;
    }
    mode = (mode & 0o7777) | base;
    if kind == FileKind::Dir {
        // directories must stay traversable
        mode |= 0o111;
    }
    mode & 0o7777
}

/// Apply the execution bit change requested by `chmod` onto the stored mode.
pub fn apply_chmod(current: u32, requested: u32, kind: FileKind) -> u32 {
    let mut mode = requested & 0o7777;
    if kind == FileKind::Dir {
        mode |= 0o111;
    }
    let _ = current;
    mode
}

/// Attribute word for the presented file, so that `readonly` in Windows is not lost.
pub fn attributes_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        0
    }
}

pub fn is_directory_attributes(attrs: u32) -> bool {
    attrs & ATTR_DIRECTORY != 0
}

pub fn is_readonly_attributes(attrs: u32) -> bool {
    attrs & ATTR_READONLY != 0
}

pub fn is_hidden_attributes(attrs: u32) -> bool {
    attrs & ATTR_HIDDEN != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_for_the_same_file_and_differs_for_others() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let ia = identity_of_path(&a, FileKind::File).unwrap();
        let ia2 = identity_of_path(&a, FileKind::File).unwrap();
        let ib = identity_of_path(&b, FileKind::File).unwrap();
        assert_eq!(ia, ia2);
        assert_ne!(ia, ib);
        assert_eq!(ia.file_id.len(), 8);
    }

    #[test]
    fn identity_follows_the_file_across_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"a").unwrap();
        let before = identity_of_path(&a, FileKind::File).unwrap();
        let moved = dir.path().join("renamed.txt");
        std::fs::rename(&a, &moved).unwrap();
        let after = identity_of_path(&moved, FileKind::File).unwrap();
        assert_eq!(before, after, "renaming must keep the file entity");
    }

    #[test]
    fn a_replacement_file_is_a_new_entity() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"a").unwrap();
        let before = identity_of_path(&a, FileKind::File).unwrap();
        // editors write a temporary file and rename it over the original
        let tmp = dir.path().join("a.txt.tmp");
        std::fs::write(&tmp, b"replaced").unwrap();
        std::fs::rename(&tmp, &a).unwrap();
        let after = identity_of_path(&a, FileKind::File).unwrap();
        assert_ne!(
            before, after,
            "a replaced file entity must get a fresh default mode"
        );
    }

    #[test]
    fn directory_enumeration_reports_kinds_and_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.cpp"), b"int main(){}").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let entries = read_dir_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);
        let main = entries.iter().find(|e| e.name == "main.cpp").unwrap();
        assert_eq!(main.kind, FileKind::File);
        assert_eq!(main.size, 12);
        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.kind, FileKind::Dir);
        assert!(sub.identity.is_some());
    }

    #[test]
    fn new_files_are_not_executable_by_default() {
        let mode = compose_mode(0, FileKind::File, false);
        assert_eq!(mode & 0o111, 0, "new source files must not be executable");
        let mode = compose_mode(0, FileKind::Dir, false);
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn chmod_transitions_execute_bits() {
        let base = compose_mode(0, FileKind::File, false);
        assert_eq!(base, 0o644);
        let exec = apply_chmod(base, 0o755, FileKind::File);
        assert_eq!(exec & 0o111, 0o111);
        let noexec = apply_chmod(exec, 0o644, FileKind::File);
        assert_eq!(noexec & 0o111, 0);
    }

    #[test]
    fn directories_stay_traversable_even_after_chmod_000() {
        let m = apply_chmod(0o755, 0o000, FileKind::Dir);
        assert_eq!(m & 0o111, 0o111);
    }
}
