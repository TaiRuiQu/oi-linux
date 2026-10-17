//! File execution mode metadata (`OILinux_PRD.md` §7.3).
//!
//! NTFS has no POSIX mode, so the share layer keeps a small SQLite table mapping a *verified*
//! file identity to the simulated mode. The identity is a triple of volume id, NTFS file id and
//! the creation time of the file entity, which keeps metadata attached to the file object itself:
//! renaming preserves it, while an editor replacing the file with a new one produces a new entity
//! with a fresh default mode.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{OiError, OiResult};

pub const DEFAULT_UMASK: u32 = 0o022;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS file_mode (
    volume_id     TEXT NOT NULL,
    file_id       BLOB NOT NULL,
    created_at_fs TEXT NOT NULL,
    mode          INTEGER NOT NULL,
    file_kind     TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (volume_id, file_id, created_at_fs)
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Dir => "dir",
        }
    }

    pub fn from_str(s: &str) -> FileKind {
        if s == "dir" {
            FileKind::Dir
        } else {
            FileKind::File
        }
    }

    /// What Linux would produce for a newly created object with the configured umask.
    pub fn default_mode(self, umask: u32) -> u32 {
        match self {
            FileKind::File => 0o666 & !umask,
            FileKind::Dir => 0o777 & !umask,
        }
    }
}

/// A verified NTFS file identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume_id: String,
    pub file_id: Vec<u8>,
    pub created_at_fs: String,
}

impl FileIdentity {
    pub fn new(volume_id: impl Into<String>, file_id: Vec<u8>, created_at_fs: impl Into<String>) -> Self {
        FileIdentity {
            volume_id: volume_id.into(),
            file_id,
            created_at_fs: created_at_fs.into(),
        }
    }
}

pub struct ModeDb {
    conn: Mutex<Connection>,
    umask: u32,
}

impl ModeDb {
    pub fn open(path: &Path) -> OiResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
        }
        let conn = Connection::open(path).map_err(|e| {
            OiError::Other(format!(
                "could not open the permission metadata database {}: {e}",
                path.display()
            ))
        })?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(|e| OiError::Other(format!("could not configure metadata.db: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| OiError::Other(format!("could not create the metadata schema: {e}")))?;
        let db = ModeDb {
            conn: Mutex::new(conn),
            umask: DEFAULT_UMASK,
        };
        db.set_meta("schema", "1")?;
        crate::sys::acl::restrict_to_current_user(path)?;
        Ok(db)
    }

    pub fn open_in_memory() -> OiResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(ModeDb {
            conn: Mutex::new(conn),
            umask: DEFAULT_UMASK,
        })
    }

    pub fn umask(&self) -> u32 {
        self.umask
    }

    /// `PRAGMA integrity_check`. A damaged database stops sharing; it is never silently repaired
    /// and the workspace directory is never removed.
    pub fn verify(&self) -> OiResult<()> {
        let conn = self.lock()?;
        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| OiError::Other(format!("integrity_check failed to run: {e}")))?;
        if result != "ok" {
            return Err(OiError::Other(format!(
                "the permission metadata database reports: {result}"
            )));
        }
        Ok(())
    }

    fn lock(&self) -> OiResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| OiError::Other("the permission metadata database lock is poisoned".into()))
    }

    fn set_meta(&self, key: &str, value: &str) -> OiResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> OiResult<Option<String>> {
        let conn = self.lock()?;
        let v = conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        Ok(v)
    }

    /// Mode of a file entity, or the Linux default for a newly seen entity of that kind.
    pub fn get_or_default(&self, id: &FileIdentity, kind: FileKind) -> OiResult<u32> {
        let conn = self.lock()?;
        let existing: Option<i64> = conn
            .query_row(
                "SELECT mode FROM file_mode WHERE volume_id = ?1 AND file_id = ?2 AND created_at_fs = ?3",
                params![id.volume_id, id.file_id, id.created_at_fs],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match existing {
            Some(m) => m as u32,
            None => kind.default_mode(self.umask),
        })
    }

    pub fn has_record(&self, id: &FileIdentity) -> OiResult<bool> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM file_mode WHERE volume_id = ?1 AND file_id = ?2 AND created_at_fs = ?3",
            params![id.volume_id, id.file_id, id.created_at_fs],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Persist a mode transitionally: either the row is written or the caller sees the error.
    pub fn set_mode(&self, id: &FileIdentity, kind: FileKind, mode: u32) -> OiResult<()> {
        let mode = mode & 0o7777;
        let now = crate::timeutil::now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO file_mode(volume_id, file_id, created_at_fs, mode, file_kind, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(volume_id, file_id, created_at_fs) DO UPDATE SET
                mode = excluded.mode,
                file_kind = excluded.file_kind,
                updated_at = excluded.updated_at",
            params![
                id.volume_id,
                id.file_id,
                id.created_at_fs,
                mode as i64,
                kind.as_str(),
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Drop metadata for an entity that no longer exists.
    pub fn forget(&self, id: &FileIdentity) -> OiResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM file_mode WHERE volume_id = ?1 AND file_id = ?2 AND created_at_fs = ?3",
            params![id.volume_id, id.file_id, id.created_at_fs],
        )?;
        Ok(())
    }

    pub fn count(&self) -> OiResult<u64> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM file_mode", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// All recorded modes of one volume, keyed by file id. Used when listing a directory so that
    /// a large workspace needs a single query instead of one per entry.
    pub fn snapshot_volume(&self, volume_id: &str) -> OiResult<std::collections::HashMap<Vec<u8>, u32>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT file_id, mode FROM file_mode WHERE volume_id = ?1")?;
        let rows = stmt.query_map(params![volume_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, mode) = row?;
            out.insert(id, mode as u32);
        }
        Ok(out)
    }
}

/// POSIX owner mapping used for the simulated attributes. The share presents a single owner; it
/// does not attempt to model Windows ACLs as POSIX users.
pub const SIMULATED_UID: u32 = 1000;
pub const SIMULATED_GID: u32 = 1000;

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> FileIdentity {
        FileIdentity::new("vol", vec![n; 16], "2026-10-16T00:00:00Z")
    }

    #[test]
    fn new_files_get_linux_default_modes() {
        let db = ModeDb::open_in_memory().unwrap();
        assert_eq!(db.get_or_default(&id(1), FileKind::File).unwrap(), 0o644);
        assert_eq!(db.get_or_default(&id(2), FileKind::Dir).unwrap(), 0o755);
        assert!(!db.has_record(&id(1)).unwrap());
    }

    #[test]
    fn chmod_is_persisted_per_entity() {
        let db = ModeDb::open_in_memory().unwrap();
        db.set_mode(&id(1), FileKind::File, 0o755).unwrap();
        assert_eq!(db.get_or_default(&id(1), FileKind::File).unwrap(), 0o755);
        db.set_mode(&id(1), FileKind::File, 0o644).unwrap();
        assert_eq!(db.get_or_default(&id(1), FileKind::File).unwrap(), 0o644);
        assert!(db.has_record(&id(1)).unwrap());
    }

    #[test]
    fn metadata_follows_the_entity_not_the_path() {
        let db = ModeDb::open_in_memory().unwrap();
        db.set_mode(&id(7), FileKind::File, 0o755).unwrap();
        // a different file id (a replacement file) starts from the default again
        assert_eq!(db.get_or_default(&id(8), FileKind::File).unwrap(), 0o644);
        // the same file id with a different creation time is also a different entity
        let other = FileIdentity::new("vol", vec![7u8; 16], "2027-01-01T00:00:00Z");
        assert_eq!(db.get_or_default(&other, FileKind::File).unwrap(), 0o644);
    }

    #[test]
    fn a_different_volume_does_not_share_metadata() {
        let db = ModeDb::open_in_memory().unwrap();
        db.set_mode(&id(1), FileKind::File, 0o755).unwrap();
        let other_volume = FileIdentity::new("other", vec![1u8; 16], "2026-10-16T00:00:00Z");
        assert_eq!(
            db.get_or_default(&other_volume, FileKind::File).unwrap(),
            0o644
        );
    }

    #[test]
    fn forget_removes_only_that_entity() {
        let db = ModeDb::open_in_memory().unwrap();
        db.set_mode(&id(1), FileKind::File, 0o755).unwrap();
        db.set_mode(&id(2), FileKind::File, 0o755).unwrap();
        db.forget(&id(1)).unwrap();
        assert_eq!(db.get_or_default(&id(1), FileKind::File).unwrap(), 0o644);
        assert_eq!(db.get_or_default(&id(2), FileKind::File).unwrap(), 0o755);
        assert_eq!(db.count().unwrap(), 1);
    }

    #[test]
    fn mode_bits_are_masked_to_posix_range() {
        let db = ModeDb::open_in_memory().unwrap();
        // everything outside the POSIX mode word is dropped, setuid/setgid/sticky are kept
        db.set_mode(&id(1), FileKind::File, 0o100_7777_755).unwrap();
        assert_eq!(db.get_or_default(&id(1), FileKind::File).unwrap(), 0o7755);
        db.set_mode(&id(2), FileKind::Dir, 0o7777).unwrap();
        assert_eq!(db.get_or_default(&id(2), FileKind::Dir).unwrap(), 0o7777);
    }

    #[test]
    fn integrity_check_passes_on_a_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let db = ModeDb::open(&path).unwrap();
        db.verify().unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn a_corrupt_database_reports_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        std::fs::write(&path, b"this is not a sqlite database at all").unwrap();
        let err = match ModeDb::open(&path) {
            Ok(_) => panic!("opening a corrupt database must fail"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("metadata") || err.to_string().contains("sqlite"),
            "{err}"
        );
    }
}
