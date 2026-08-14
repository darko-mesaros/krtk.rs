use lambda_http::http::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Validation error: {0}")]
    Validation(String),

    #[error("URL not found: {0}")]
    NotFound(String),

    #[error("Data store operation failed")]
    Database(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Data serialization failed")]
    Serialization(#[from] serde_dynamo::Error),

    #[error("URL safety check failed: {0}")]
    SafeBrowsing(String),

    #[error("Failed to render response")]
    Template(#[from] askama::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation(_) | Self::SafeBrowsing(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn database<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Self::Database(Box::new(err))
    }
}
