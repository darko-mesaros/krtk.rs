use lambda_http::http::StatusCode;
use lambda_http::{Body, Error, Response};

use serde::Serialize;

use crate::error::AppError;

// Redirect response
// TODO: Handle if the url has no http/https in front
pub fn redirect_response(location: &str) -> Result<Response<Body>, Error> {
    // Generate a redirect response
    let response = Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", location) // Set the location (URL) to whatever we tell it to
        .body(Body::Empty) // No need for a body here
        .map_err(Box::new)?; // Converting the builder error into the lambda_http::Error

    Ok(response)
}

// Just return an empty response of the same status
pub fn empty_response(status: &StatusCode) -> Result<Response<Body>, Error> {
    let response = Response::builder()
        .status(status)
        .body(Body::Empty)
        .map_err(Box::new)?;

    Ok(response)
}

// Respond with JSON
// Takes in some body that implements the Serialize trait
pub fn json_response(status: &StatusCode, body: &impl Serialize) -> Result<Response<Body>, Error> {
    let response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::Text(
            // Serialize the body into a JSON string
            serde_json::to_string(&body).unwrap(), // TODO: Handle the unwrap
        ))
        .map_err(Box::new)?;

    Ok(response)
}
// Respond with HTML
// Takes in some body that implements the Serialize trait
pub fn html_response(status: &StatusCode, body: String) -> Result<Response<Body>, Error> {
    let response = Response::builder()
        .status(status)
        .header("content-type", "text/html")
        .body(Body::Text(body))
        .map_err(Box::new)?;

    Ok(response)
}

/// Respond with the status an [`AppError`] maps to, and a JSON `{"error": ...}` body.
///
/// Client errors (4xx) return the error's own message, because the caller can act on
/// it — "Invalid URL Provided" is useful, and the auth variants are already worded to
/// reveal nothing (see `error.rs`).
///
/// Server errors (5xx) return a **fixed generic message** instead of the error's own.
/// Variants like `Internal(String)` interpolate their argument, so echoing them would
/// eventually leak a table name, an ARN, or an SDK error chain into a public response.
/// The real error still goes to CloudWatch via the caller's `tracing::error!`.
pub fn error_response(err: &AppError) -> Result<Response<Body>, Error> {
    let status = err.status_code();

    let message = if status.is_server_error() {
        "Something went wrong".to_string()
    } else {
        err.to_string()
    };

    let response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::Text(
            serde_json::json!({ "error": message }).to_string(),
        ))
        .map_err(Box::new)?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_returns_401_with_its_own_message() {
        let resp = error_response(&AppError::Unauthorized).unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        match resp.body() {
            Body::Text(t) => assert!(t.contains("Authentication required"), "got {t}"),
            other => panic!("expected a text body, got {other:?}"),
        }
    }

    #[test]
    fn forbidden_returns_403() {
        let resp = error_response(&AppError::Forbidden).unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn client_errors_keep_their_actionable_message() {
        let resp = error_response(&AppError::Validation("Invalid URL Provided".into())).unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        match resp.body() {
            Body::Text(t) => assert!(t.contains("Invalid URL Provided"), "got {t}"),
            other => panic!("expected a text body, got {other:?}"),
        }
    }

    /// A 500 must not echo the variant's interpolated detail to the caller.
    #[test]
    fn server_errors_do_not_leak_their_detail() {
        let resp =
            error_response(&AppError::Internal("linkTable-prod-abc123 timed out".into())).unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        match resp.body() {
            Body::Text(t) => {
                assert!(!t.contains("linkTable"), "internal detail leaked: {t}");
                assert!(t.contains("Something went wrong"), "got {t}");
            }
            other => panic!("expected a text body, got {other:?}"),
        }
    }
}
