use std::path::PathBuf;

/// Result alias used across the host core.
pub type OiResult<T> = Result<T, OiError>;

#[derive(Debug, thiserror::Error)]
pub enum OiError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    PlainIo(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("path is outside of the shared root: {0}")]
    PathEscapesRoot(String),

    #[error("path uses a construct that is not allowed in the shared root: {0}")]
    PathNotAllowed(String),

    #[error("the requested file identity could not be verified for {0}")]
    IdentityUnverifiable(String),

    #[error("shared root is unavailable: {0}")]
    ShareUnavailable(String),

    #[error("file is locked by another process: {0}")]
    FileLocked(String),

    #[error("virtual machine is not in a state that allows this operation (state = {0})")]
    InvalidState(String),

    #[error("virtual machine management error: {0}")]
    Vm(String),

    #[error("ssh error: {0}")]
    Ssh(String),

    #[error("share bridge error: {0}")]
    Bridge(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("environment image verification failed: {0}")]
    ImageVerification(String),

    #[error("not initialised: {0}")]
    NotInitialized(String),

    #[error("operation timed out: {0}")]
    Timeout(String),

    #[error("{0}")]
    Other(String),
}

impl From<russh::Error> for OiError {
    fn from(e: russh::Error) -> Self {
        OiError::Ssh(e.to_string())
    }
}

impl OiError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        OiError::Io {
            path: path.into(),
            source,
        }
    }
    pub fn other(msg: impl Into<String>) -> Self {
        OiError::Other(msg.into())
    }

    pub fn vm(msg: impl Into<String>) -> Self {
        OiError::Vm(msg.into())
    }

    pub fn ssh(msg: impl Into<String>) -> Self {
        OiError::Ssh(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        OiError::Config(msg.into())
    }

    pub fn timeout(msg: impl Into<String>) -> Self {
        OiError::Timeout(msg.into())
    }

    /// True when the error describes a *share* failure, which by product rule must never
    /// block the use of the Linux environment.
    pub fn is_share_error(&self) -> bool {
        matches!(
            self,
            OiError::ShareUnavailable(_)
                | OiError::PathEscapesRoot(_)
                | OiError::PathNotAllowed(_)
                | OiError::IdentityUnverifiable(_)
                | OiError::FileLocked(_)
                | OiError::Bridge(_)
        )
    }
}

/// `sqlx`-free helper: map a rusqlite error into an [`OiError`] with context.
pub fn sqlite_ctx<T>(res: Result<T, rusqlite::Error>, ctx: &str) -> OiResult<T> {
    res.map_err(|e| OiError::Other(format!("sqlite ({ctx}): {e}")))
}
