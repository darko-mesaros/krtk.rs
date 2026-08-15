use lambda_http::http::StatusCode;
use lambda_http::{run, service_fn, tracing, Error, IntoResponse, Request, RequestExt};

use shared::auth::owner_from_request;
use shared::core::UrlShortener;
use shared::response::{empty_response, error_response, json_response, html_response};
use shared::templates::{LinksTable, Link, Template};

use std::env;

// The main bit of code that will run every time this function is triggered
async fn function_handler(
    url_shortener: &UrlShortener,
    event: Request,
) -> Result<impl IntoResponse, Error> {
    // Tracing
    tracing::info!("Received event: {:?}", event);

    // Identity comes from the authorizer context, never from the request itself.
    // Behind a configured authorizer this cannot fail; if it does, the route is
    // misconfigured and failing closed is the only safe answer.
    let owner_sub = match owner_from_request(&event) {
        Ok(sub) => sub,
        Err(e) => {
            tracing::error!("rejecting list request without owner identity: {:?}", e);
            return error_response(&e);
        }
    };

    // Get the query parameters from the event
    let query_params = event.query_string_parameters();
    // Search for last_evaluated_id and store it into the var
    let last_evaluated_id = query_params.first("last_evaluated_id");
    let last_evaluated_timestamp = query_params.first("last_evaluated_timestamp");

    // Only this owner's links. Scoping is in the query's partition key, so another
    // owner's items are never read rather than being read and filtered.
    let links = url_shortener
        .list_urls(&owner_sub, last_evaluated_id, last_evaluated_timestamp)
        .await;

    // See if the request is coming from the front end HTMX
    let htmx_request = event.headers().get("Hx-Request");

    // Handle the links
    match links {
        Ok(links) if htmx_request.is_some() => {
                tracing::info!("Request is HTMX");
                // TODO: Make this more compact and handle the Results
                let links_str = serde_json::to_value(&links)?;
                let table_links: Vec<Link> = serde_json::from_value(links_str["short_urls"].clone())?;
                let table_html = LinksTable {
                    links: table_links,
                    // TODO: Make this not hardcoded
                    domain: "krtk.rs/",
                    has_more: links.has_more,
                };
                let body = table_html.render()?; // Render HTML
                html_response(&StatusCode::OK, body) // Respond with HTML
        },
        Ok(links) => json_response(&StatusCode::OK, &links),
        Err(e) => {
            tracing::error!("Failed to list URLs 🔥 : {:?}", e);
            empty_response(&StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();

    // Get the table name from the env variables
    let table_name = env::var("TABLE_NAME").expect("No TABLE_NAME environment variable set");
    let shortener_domain = env::var("SHORTENER_DOMAIN").expect("No SHORTENER_DOMAIN environment variable set");
    // Set up the AWS DynamoDB SDK Client
    let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12()).load().await;
    let dynamodb_client = aws_sdk_dynamodb::Client::new(&config);

    let shortener = UrlShortener::new(&table_name, &shortener_domain, dynamodb_client);

    run(service_fn(|event| function_handler(&shortener, event))).await
}
