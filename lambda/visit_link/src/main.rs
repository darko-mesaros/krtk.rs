use lambda_http::http::StatusCode;
use lambda_http::{run, service_fn, tracing, Error, IntoResponse, Request, RequestExt};

use shared::core::UrlShortener;
use shared::response::{empty_response, redirect_response};

use std::env;

// The main bit of code that will run every time this function is triggered
async fn function_handler(
    url_shortener: &UrlShortener,
    event: Request,
) -> Result<impl IntoResponse, Error> {
    // Tracing
    tracing::info!("Received event: {:?}", event);
    // Try to get link ID, if there is none, just return empty
    let link_id = event
        .path_parameters_ref()
        .and_then(|params| params.first("linkId"))
        .unwrap_or(""); // TODO: Should be an Option

    if link_id.is_empty() {
        return empty_response(&StatusCode::NOT_FOUND);
    }

    let full_url = url_shortener
        .retrieve_url(link_id)
        .await;

    match full_url {
        Err(e) => {
            tracing::error!("Failed to retrieve URL 🧨 : {:?}", e);
            empty_response(&StatusCode::INTERNAL_SERVER_ERROR)
        }
        Ok(None) => empty_response(&StatusCode::NOT_FOUND),
        Ok(Some(url)) => redirect_response(&url),
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();

    // Get the table name from the env variables
    let table_name = env::var("TABLE_NAME").expect("No TABLE_NAME environment variable set");
    // Set up the AWS DynamoDB SDK Client
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let dynamodb_client = aws_sdk_dynamodb::Client::new(&config);

    let shortener = UrlShortener::new(&table_name, dynamodb_client);

    run(service_fn(|event| function_handler(&shortener, event))).await
}
