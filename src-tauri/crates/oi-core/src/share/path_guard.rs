//! Path validation and containment for the shared root.
//!
//! `OILinux_PRD.md` §7.1 and §8.3 require that no SFTP request can reach anything outside the
//! single configured NTFS root, and explicitly list the constructs that must be refused:
//! relative escapes (`.`/`..`), absolute paths, reparse points (symlinks, junctions), Windows
//! special paths, path separator variants and NTFS alternate data streams.
//!
//! The strategy is defence in depth:
//!
//! 1. **Lexical**: the SFTP path is split on both separators and every component is validated
//!    before anything touches the file system. `..`, `:`, reserved device names, trailing dots
//!    and spaces, and `\\?\`/`\\.\` prefixes are rejected here.
//! 2. **Containment**: the candidate path is canonicalised, which resolves every reparse point
//!    on the way, and the result must still live under the canonical root.
//! 3. **Handle**: every open uses `FILE_FLAG_OPEN_REPARSE_POINT` and the resulting file's
//!    attribute word is inspected, so a link swapped in after step 2 is still refused rather
//!    than silently followed.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{OiError, OiResult};

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A validated shared root. Construct once per service lifetime.
#[derive(Debug, Clone)]
pub struct ShareRoot {
    configured: PathBuf,
    canonical: PathBuf,
}

impl ShareRoot {
    pub fn new(root: &Path) -> OiResult<Self> {
        if !root.exists() {
            return Err(OiError::ShareUnavailable(format!(
                "the shared root {} does not exist",
                root.display()
            )));
        }
        if !root.is_dir() {
            return Err(OiError::ShareUnavailable(format!(
                "the shared root {} is not a directory",
                root.display()
            )));
        }
        if is_reparse_point(root)? {
            return Err(OiError::PathNotAllowed(format!(
                "the shared root {} is a reparse point; choose a real folder",
                root.display()
            )));
        }
        let canonical = std::fs::canonicalize(root).map_err(|e| OiError::io(root, e))?;
        Ok(ShareRoot {
            configured: root.to_path_buf(),
            canonical,
        })
    }

    pub fn configured(&self) -> &Path {
        &self.configured
    }

    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// Validate an SFTP path lexically and translate it into a Windows path under the root.
    ///
    /// `sftp_path` is the raw string from the SFTP request. A leading `/` means "relative to the
    /// shared root", which is how OpenSSH's sftp-server interprets it too. `.` components are
    /// dropped and `..` pops the previous component; a pop with nothing left to pop is the
    /// escape case and is refused, so `a/../b` is legal while `../a` and `a/../../b` are not.
    pub fn resolve(&self, sftp_path: &str) -> OiResult<PathBuf> {
        let components = self.split_and_validate(sftp_path)?;
        let mut out = self.configured.clone();
        for c in components {
            out.push(c);
        }
        Ok(out)
    }

    /// Validate and split an SFTP path into safe components.
    pub fn split_and_validate(&self, sftp_path: &str) -> OiResult<Vec<OsString>> {
        if sftp_path.is_empty() {
            return Ok(Vec::new());
        }
        if sftp_path.contains('\0') {
            return Err(OiError::PathNotAllowed(
                "the request contains a NUL byte".into(),
            ));
        }
        if sftp_path.len() > 4096 {
            return Err(OiError::PathNotAllowed("the path is unreasonably long".into()));
        }
        if sftp_path.starts_with("\\\\") || sftp_path.starts_with("//") {
            return Err(OiError::PathNotAllowed(format!(
                "UNC and device paths are not allowed: {sftp_path}"
            )));
        }
        let lowered = sftp_path.to_ascii_lowercase();
        if lowered.contains(r"\\?\") || lowered.contains(r"\\.\") || lowered.contains("//?/") {
            return Err(OiError::PathNotAllowed(format!(
                "Windows special paths are not allowed: {sftp_path}"
            )));
        }

        let mut out: Vec<OsString> = Vec::new();
        for raw in sftp_path.split(['/', '\\']) {
            if raw.is_empty() || raw == "." {
                continue;
            }
            if raw == ".." {
                if out.pop().is_none() {
                    return Err(OiError::PathEscapesRoot(format!(
                        "the request climbs above the shared root: {sftp_path}"
                    )));
                }
                continue;
            }
            validate_component(raw, sftp_path)?;
            out.push(OsString::from(raw));
        }
        Ok(out)
    }

    /// Canonicalise `path` and require that it is still inside the root.
    ///
    /// For paths that do not exist yet, the parent directory is checked instead.
    pub fn verify_contained(&self, path: &Path) -> OiResult<PathBuf> {
        let target = if path.exists() {
            path.to_path_buf()
        } else {
            path.parent()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| OiError::PathEscapesRoot(path.display().to_string()))?
        };
        let canonical = std::fs::canonicalize(&target).map_err(|e| {
            OiError::PathNotAllowed(format!(
                "the path {} cannot be resolved ({e})",
                target.display()
            ))
        })?;
        if !is_under(&self.canonical, &canonical) {
            return Err(OiError::PathEscapesRoot(format!(
                "{} resolves to {}, which is outside the shared root",
                path.display(),
                canonical.display()
            )));
        }
        Ok(canonical)
    }

    /// Reject the path if any existing component is a reparse point. Used before creating or
    /// deleting so that a junction cannot redirect the operation.
    pub fn reject_reparse_in_existing_ancestors(&self, path: &Path) -> OiResult<()> {
        let mut current = Some(path);
        while let Some(p) = current {
            if p.exists() && is_reparse_point(p)? {
                return Err(OiError::PathNotAllowed(format!(
                    "{} is a reparse point (symlink or junction), which the share refuses to follow",
                    p.display()
                )));
            }
            if self.canonical.starts_with(p) && p == self.configured {
                break;
            }
            current = p.parent();
        }
        Ok(())
    }
}

fn validate_component(raw: &str, full: &str) -> OiResult<()> {
    if raw.contains(':') {
        return Err(OiError::PathNotAllowed(format!(
            "alternate data streams and drive letters are not allowed: {raw}"
        )));
    }
    if raw
        .chars()
        .any(|c| matches!(c, '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(OiError::PathNotAllowed(format!(
            "wildcards and reserved characters are not allowed: {raw}"
        )));
    }
    if raw.chars().any(|c| (c as u32) < 0x20) {
        return Err(OiError::PathNotAllowed(
            "control characters are not allowed in names".into(),
        ));
    }
    if raw.ends_with(' ') || raw.ends_with('.') {
        return Err(OiError::PathNotAllowed(format!(
            "names ending in a space or a dot are ambiguous on NTFS: {raw} (in {full})"
        )));
    }
    let stem = raw.split('.').next().unwrap_or(raw);
    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(OiError::PathNotAllowed(format!(
            "reserved Windows device names are not allowed: {raw}"
        )));
    }
    Ok(())
}

/// Does `child` live inside `root`? Comparison is case-insensitive on Windows and happens on the
/// canonical (already `\\?\`-prefixed) forms.
pub fn is_under(root: &Path, child: &Path) -> bool {
    let r = normalize_for_compare(root);
    let c = normalize_for_compare(child);
    if r == c {
        return true;
    }
    let mut prefix = r.clone();
    if !prefix.ends_with(['\\', '/']) {
        prefix.push('\\');
    }
    c.starts_with(&prefix) || c.starts_with(&prefix.replace('\\', "/"))
}

fn normalize_for_compare(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    let s = s.replace('/', "\\");
    s.trim_end_matches('\\').to_ascii_lowercase()
}

/// Is `path` a symlink, junction or any other reparse point?
pub fn is_reparse_point(path: &Path) -> OiResult<bool> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| OiError::io(path, e))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        Ok(meta.file_type().is_symlink())
    }
}

/// Open a path with `FILE_FLAG_OPEN_REPARSE_POINT`, so the handle refers to the link itself
/// instead of its target, then refuse to continue when that object turns out to be a link.
pub fn open_no_follow(options: &mut std::fs::OpenOptions, path: &Path) -> OiResult<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| {
        if is_lock_error(&e) {
            OiError::FileLocked(format!(
                "{} is locked by another process and cannot be replaced or written right now",
                path.display()
            ))
        } else {
            OiError::io(path, e)
        }
    })?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let attrs = file.metadata().map(|m| m.file_attributes()).unwrap_or(0);
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(OiError::PathNotAllowed(format!(
                "{} is a reparse point, which the share refuses to follow",
                path.display()
            )));
        }
    }
    Ok(file)
}

fn is_lock_error(e: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION (32) / ERROR_LOCK_VIOLATION (33)
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// Normalise an SFTP path for display and comparisons inside the protocol handler.
pub fn sftp_display(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 1);
    if !path.starts_with('/') {
        out.push('/');
    }
    out.push_str(path);
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Reject a name that would be a relative component when creating an entry.
pub fn validate_new_name(name: &str) -> OiResult<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(OiError::PathNotAllowed(format!("invalid entry name: {name:?}")));
    }
    if name.contains(['/', '\\', ':']) {
        return Err(OiError::PathNotAllowed(format!(
            "entry names must not contain separators or colons: {name}"
        )));
    }
    Ok(())
}

/// Convert a Windows path into the SFTP view (forward slashes, rooted at the share).
pub fn to_sftp_path(root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    format!("/{relative}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, ShareRoot) {
        let dir = tempfile::tempdir().unwrap();
        let r = ShareRoot::new(dir.path()).unwrap();
        (dir, r)
    }

    #[test]
    fn plain_paths_resolve_under_the_root() {
        let (dir, r) = root();
        let p = r.resolve("sub/main.cpp").unwrap();
        assert_eq!(p, dir.path().join("sub").join("main.cpp"));
        let p = r.resolve("/sub/main.cpp").unwrap();
        assert_eq!(p, dir.path().join("sub").join("main.cpp"));
        let p = r.resolve("").unwrap();
        assert_eq!(p, dir.path());
    }

    #[test]
    fn traversal_above_the_root_is_refused_in_every_spelling() {
        let (_d, r) = root();
        for bad in [
            "../etc/passwd",
            "a/../../b",
            "..\\windows\\system32",
            "/../secrets",
            "./../x",
            "..",
            "a/b/../../../c",
        ] {
            let err = r.resolve(bad).unwrap_err();
            assert!(
                matches!(err, OiError::PathEscapesRoot(_) | OiError::PathNotAllowed(_)),
                "{bad} produced {err}"
            );
        }
    }

    #[test]
    fn traversal_that_stays_inside_is_normalised() {
        let (dir, r) = root();
        assert_eq!(r.resolve("a/../b").unwrap(), dir.path().join("b"));
        assert_eq!(r.resolve("a/..").unwrap(), dir.path());
        assert_eq!(r.resolve("./a/./b").unwrap(), dir.path().join("a").join("b"));
        assert_eq!(r.resolve("a/b/..").unwrap(), dir.path().join("a"));
    }

    #[test]
    fn alternate_data_streams_and_drive_letters_are_refused() {
        let (_d, r) = root();
        for bad in ["file.txt:secret", "C:/windows", "a/b:stream", "/x:$DATA"] {
            assert!(r.resolve(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn special_windows_paths_are_refused() {
        let (_d, r) = root();
        for bad in [
            r"\\?\C:\windows",
            r"\\.\PhysicalDrive0",
            r"\\server\share\file",
            "//server/share",
        ] {
            assert!(r.resolve(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn device_names_are_refused() {
        let (_d, r) = root();
        for bad in ["CON", "nul", "aux.txt", "COM1", "sub/LPT1"] {
            assert!(r.resolve(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn trailing_dots_and_spaces_are_refused() {
        let (_d, r) = root();
        for bad in ["name.", "name ", "sub/name. "] {
            assert!(r.resolve(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn containment_check_rejects_an_escaping_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"top secret").unwrap();
        let r = ShareRoot::new(dir.path()).unwrap();

        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        );
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        );
        if created.is_err() {
            // creating symlinks needs privileges on Windows; the lexical tests still cover the
            // refusal paths.
            return;
        }
        assert!(is_reparse_point(&dir.path().join("link.txt")).unwrap());
        let err = r.verify_contained(&dir.path().join("link.txt")).unwrap_err();
        assert!(matches!(err, OiError::PathEscapesRoot(_)), "{err}");
        let err = r.resolve("link.txt").and_then(|p| {
            open_no_follow(&mut std::fs::OpenOptions::new().read(true), &p).map(|_| p)
        });
        assert!(err.is_err());
    }

    #[test]
    fn nested_directory_symlinks_are_caught_by_canonicalisation() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"x").unwrap();
        let r = ShareRoot::new(dir.path()).unwrap();
        #[cfg(windows)]
        let created =
            std::os::windows::fs::symlink_dir(outside.path(), dir.path().join("escape"));
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(outside.path(), dir.path().join("escape"));
        if created.is_err() {
            return;
        }
        // lexical check passes, containment check must fail
        let candidate = r.resolve("escape/secret.txt").unwrap();
        let err = r.verify_contained(&candidate).unwrap_err();
        assert!(matches!(err, OiError::PathEscapesRoot(_)), "{err}");
    }

    #[test]
    fn is_under_is_case_insensitive_and_prefix_safe() {
        let base = Path::new(r"\\?\C:\Work\Oi");
        assert!(is_under(base, Path::new(r"\\?\C:\work\oi\main.cpp")));
        assert!(is_under(base, Path::new(r"\\?\C:\Work\Oi")));
        assert!(!is_under(base, Path::new(r"\\?\C:\Work\OiOthe\rx")));
        assert!(!is_under(base, Path::new(r"\\?\C:\Other")));
    }

    #[test]
    fn sftp_display_normalises_separators_and_trailing_slashes() {
        assert_eq!(sftp_display("a/b/"), "/a/b");
        assert_eq!(sftp_display("/a"), "/a");
        assert_eq!(sftp_display(""), "/");
    }

    #[test]
    fn new_names_cannot_smuggle_separators() {
        assert!(validate_new_name("main.cpp").is_ok());
        assert!(validate_new_name("..").is_err());
        assert!(validate_new_name("a/b").is_err());
        assert!(validate_new_name("a:b").is_err());
        assert!(validate_new_name("").is_err());
    }
}
