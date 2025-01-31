use lambda_http::http::StatusCode;
use lambda_http::{Body, Error, Response};

use serde::Serialize;

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
