#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{context}: {source}")]
    DbContext {
        context: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("timestamp formatting failed: {0}")]
    Timestamp(#[from] time::error::Format),
    #[error("corruption detected: {0}")]
    Corruption(String),
    #[error("secret detected: {0}")]
    SecretDetected(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    General(String),
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
