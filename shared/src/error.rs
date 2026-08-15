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

    /// No usable credential was presented, or the one presented did not verify.
    ///
    /// The message is deliberately opaque: it must not reveal whether a token was
    /// malformed, expired, or signed by the wrong issuer, nor whether an API key
    /// exists, since that turns an error response into an oracle.
    #[error("Authentication required")]
    Unauthorized,

    /// The caller authenticated successfully but is not permitted this operation.
    ///
    /// Used when a credential type is valid but out of scope for the route (an API
    /// key reaching key management, per FR-4.4) and for owner mismatches. It never
    /// distinguishes "exists but is not yours" from "does not exist" — doing so
    /// would let a caller probe for other users' link IDs.
    #[error("Not permitted")]
    Forbidden,
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation(_) | Self::SafeBrowsing(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn database<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Self::Database(Box::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_maps_to_401() {
        assert_eq!(AppError::Unauthorized.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn forbidden_maps_to_403() {
        assert_eq!(AppError::Forbidden.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn existing_mappings_unchanged() {
        assert_eq!(
            AppError::Validation("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::NotFound("x".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AppError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// The auth error messages must not leak why verification failed, or whether a
    /// key exists. Asserting the exact strings keeps a future "helpful" edit honest.
    #[test]
    fn auth_error_messages_are_opaque() {
        assert_eq!(AppError::Unauthorized.to_string(), "Authentication required");
        assert_eq!(AppError::Forbidden.to_string(), "Not permitted");
    }
}
