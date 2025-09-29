use lambda_http::http::StatusCode;
use lambda_http::{run, service_fn, tracing, Error, IntoResponse, Request, RequestPayloadExt};

use shared::core::{ShortenUrlRequest, UrlShortener};
use shared::response::{empty_response, json_response, html_response};
use shared::url_info::UrlInfo;
use shared::templates::{NewShortLink, ErrorPopup, Template};

use std::env;

// The main bit of code that will run every time this function is triggered
async fn function_handler(
    url_shortener: &UrlShortener,
    url_info: &UrlInfo,
    event: Request,
) -> Result<impl IntoResponse, Error> {
    // Tracing
    tracing::info!("Received event: {:?}", event);

    // Get the Request
    let shorten_url_request_body = event.payload::<ShortenUrlRequest>()?;

    let htmx_request = event.headers().get("Hx-Request");
    match shorten_url_request_body {
        // If it cannot parse the payload (no "url_to_shorten")
        None => empty_response(&StatusCode::BAD_REQUEST),
        // Was able to parse the payload, lets shorten it
        Some(shorten_url_request) => {
            match shorten_url_request.validate() {
                Ok(ser) => {
                    let shortened_url_response = url_shortener
                        .shorten_url(ser, url_info)
                        .await;

                    // See if the request is coming from the front end HTMX
                    //let htmx_request = event.headers().get("Hx-Request");

                    // See if we managed to shorten it
                    match shortened_url_response {
                        Ok(response) if htmx_request.is_some() => {
                            tracing::info!("Request is HTMX");
                            let new_link_html = NewShortLink {
                                link: response.link_id,
                                // TODO: Make this not hardcoded
                                domain: "krtk.rs/"
                            };
                            let body = new_link_html.render()?; // Render HTML
                            html_response(&StatusCode::OK, body) // Respond with HTML
                        },
                        // Yes, return the JSON back
                        Ok(response) => json_response(&StatusCode::OK, &response),
                        // No, fail spectacularly
                        Err(e) if htmx_request.is_some() => {
                            tracing::error!("Failed to shorten URL 💥 : {:?}", e);
                            let error_html = ErrorPopup {
                                message: e,
                            };
                            let body = error_html.render()?; // Render HTML
                            html_response(&StatusCode::OK, body) // Respond with HTML
                        },
                        Err(e) => {
                            tracing::error!("Failed to shorten URL 💥 : {:?}", e);
                            empty_response(&StatusCode::INTERNAL_SERVER_ERROR)
                        }
                    }
                },
                Err(e) if htmx_request.is_some() => {
                    tracing::error!("Failed to validate URL 💥 : {:?}", e);
                    let error_html = ErrorPopup {
                        message: e,
                    };
                    let body = error_html.render()?; // Render HTML
                    html_response(&StatusCode::OK, body) // Respond with HTML
                },
                Err(e) => {
                    tracing::error!("Failed to validate URL 💥 : {:?}", e);
                    empty_response(&StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }
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

    // Http Client for retrieving additional information from the posted URLs
    // NOTE: We are using the shared reqwest from the shared library - re:export
    let http_client = shared::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    // Instantiate UrlInfo
    let url_info = UrlInfo::new(http_client);

    // Creating a new UrlShortener struct with defaults
    let shortener = UrlShortener::new(&table_name, dynamodb_client);

    run(service_fn(|event| {
        function_handler(&shortener, &url_info, event)
    }))
    .await
}
