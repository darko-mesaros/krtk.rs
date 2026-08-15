use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

mod apikey;
mod jwks;
mod jwt;

// --- Event / Response types for HTTP API v2 Lambda REQUEST authorizer ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizerEvent {
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub http: HttpContext,
}

#[derive(Debug, Deserialize)]
pub struct HttpContext {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizerResponse {
    pub is_authorized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<AuthorizerContext>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizerContext {
    pub owner_id: String,
    pub auth_method: String,
}

impl AuthorizerResponse {
    pub fn allow(owner_id: String, auth_method: &str) -> Self {
        Self {
            is_authorized: true,
            context: Some(AuthorizerContext {
                owner_id,
                auth_method: auth_method.to_string(),
            }),
        }
    }

    pub fn deny() -> Self {
        Self {
            is_authorized: false,
            context: None,
        }
    }
}

// --- Handler state ---

struct AppState {
    jwks_cache: jwks::JwksCache,
    cognito_pool_id: String,
    cognito_client_id: String,
    cognito_region: String,
    dynamodb_client: aws_sdk_dynamodb::Client,
    api_key_table: String,
}

async fn function_handler(
    state: &AppState,
    event: LambdaEvent<AuthorizerEvent>,
) -> Result<AuthorizerResponse, Error> {
    let headers = &event.payload.headers;

    // Try JWT path first. A Bearer token takes precedence over an API key when both are
    // presented, so a browser session cannot be silently downgraded by a stray header.
    if let Some(auth_header) = headers
        .get("authorization")
        .or_else(|| headers.get("Authorization"))
        && let Some(token) = auth_header.strip_prefix("Bearer ")
    {
        return handle_jwt(state, token.trim()).await;
    }

    // Try API key path
    if let Some(api_key) = headers.get("x-api-key").or_else(|| headers.get("X-Api-Key")) {
        return handle_api_key(state, api_key).await;
    }

    // No credential presented
    tracing::info!("No credential presented, denying request");
    Ok(AuthorizerResponse::deny())
}

async fn handle_jwt(state: &AppState, token: &str) -> Result<AuthorizerResponse, Error> {
    let issuer = format!(
        "https://cognito-idp.{}.amazonaws.com/{}",
        state.cognito_region, state.cognito_pool_id
    );

    match jwt::validate_token(
        token,
        &state.jwks_cache,
        &issuer,
        &state.cognito_client_id,
    )
    .await
    {
        Ok(claims) => {
            tracing::info!("JWT verified for sub={}", claims.sub);
            Ok(AuthorizerResponse::allow(claims.sub, "jwt"))
        }
        Err(e) => {
            tracing::warn!("JWT verification failed: {e}");
            Ok(AuthorizerResponse::deny())
        }
    }
}

async fn handle_api_key(state: &AppState, key: &str) -> Result<AuthorizerResponse, Error> {
    match apikey::verify_api_key(&state.dynamodb_client, &state.api_key_table, key).await {
        Ok(Some(owner_id)) => {
            tracing::info!("API key verified for owner={owner_id}");
            Ok(AuthorizerResponse::allow(owner_id, "apikey"))
        }
        Ok(None) => {
            tracing::warn!("API key verification failed");
            Ok(AuthorizerResponse::deny())
        }
        Err(e) => {
            tracing::error!("Key verification error: {e}");
            Ok(AuthorizerResponse::deny())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let cognito_pool_id = env::var("COGNITO_POOL_ID").expect("COGNITO_POOL_ID not set");
    let cognito_client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID not set");
    let cognito_region = env::var("COGNITO_REGION").expect("COGNITO_REGION not set");
    let api_key_table = env::var("API_KEY_TABLE_NAME").expect("API_KEY_TABLE_NAME not set");

    let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12()).load().await;
    let dynamodb_client = aws_sdk_dynamodb::Client::new(&config);

    let jwks_url = format!(
        "https://cognito-idp.{}.amazonaws.com/{}/.well-known/jwks.json",
        cognito_region, cognito_pool_id
    );
    let jwks_cache = jwks::JwksCache::new(&jwks_url);

    let state = Arc::new(AppState {
        jwks_cache,
        cognito_pool_id,
        cognito_client_id,
        cognito_region,
        dynamodb_client,
        api_key_table,
    });

    run(service_fn(|event| {
        let state = Arc::clone(&state);
        async move { function_handler(&state, event).await }
    }))
    .await
}
