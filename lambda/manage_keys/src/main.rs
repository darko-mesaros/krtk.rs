use std::env;

use aws_sdk_dynamodb::types::AttributeValue;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use lambda_http::http::StatusCode;
use lambda_http::request::RequestContext;
use lambda_http::{run, service_fn, tracing, Error, Request, RequestExt};
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use shared::auth::owner_from_request;
use shared::error::AppError;
use shared::response::{empty_response, error_response, json_response};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MintRequest {
    label: String,
    expires_in_days: Option<u32>,
}

#[derive(Serialize)]
struct MintResponse {
    key: String,
    key_id: String,
    prefix: String,
    label: String,
    created_at: i64,
    expires_at: Option<i64>,
}

#[derive(Serialize)]
struct KeySummary {
    key_id: String,
    prefix: String,
    label: String,
    created_at: i64,
    last_used_at: Option<i64>,
    expires_at: Option<i64>,
}

#[derive(Serialize)]
struct ListResponse {
    keys: Vec<KeySummary>,
}

// ---------------------------------------------------------------------------
// Key generation helpers
// ---------------------------------------------------------------------------

const KEY_PREFIX: &str = "krtk_";
const KEY_RANDOM_BYTES: usize = 32;
const KEY_PREFIX_LEN: usize = 12;
const MAX_KEYS_PER_OWNER: usize = 10;
const MAX_EXPIRY_DAYS: u32 = 365;

/// Validates a requested expiry window and converts it to an absolute unix timestamp.
///
/// `None` means the key never expires, which is the default. Pulled out of the request
/// handler so the boundary conditions are testable without a DynamoDB client or an HTTP
/// event — the previous inline version could only be exercised by a live call, which is
/// why its "tests" asserted on constants instead of behaviour.
fn expiry_from_days(
    expires_in_days: Option<u32>,
    now: i64,
) -> Result<Option<i64>, AppError> {
    match expires_in_days {
        None => Ok(None),
        Some(0) => Err(AppError::Validation(
            "expires_in_days must be a positive number".into(),
        )),
        Some(days) if days > MAX_EXPIRY_DAYS => Err(AppError::Validation(format!(
            "expires_in_days must not exceed {MAX_EXPIRY_DAYS}"
        ))),
        Some(days) => Ok(Some(now + (days as i64 * 86_400))),
    }
}

/// Generate a new API key: `krtk_` + 43 chars of base64url-encoded random bytes.
fn generate_key() -> String {
    let mut buf = [0u8; KEY_RANDOM_BYTES];
    OsRng.try_fill_bytes(&mut buf).expect("OS RNG failed");
    let encoded = URL_SAFE_NO_PAD.encode(buf);
    format!("{KEY_PREFIX}{encoded}")
}

/// SHA-256 hex digest of the plaintext key.
fn hash_key(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

/// First 12 characters of the key (e.g. "krtk_3f9aQ2x").
fn key_prefix(plaintext: &str) -> String {
    plaintext.chars().take(KEY_PREFIX_LEN).collect()
}

// ---------------------------------------------------------------------------
// DynamoDB client wrapper
// ---------------------------------------------------------------------------

struct KeyStore {
    client: aws_sdk_dynamodb::Client,
    table_name: String,
}

impl KeyStore {
    fn new(table_name: &str, client: aws_sdk_dynamodb::Client) -> Self {
        Self {
            client,
            table_name: table_name.to_string(),
        }
    }

    /// Count existing keys for the owner using the OwnerIndex GSI.
    async fn count_owner_keys(&self, owner_id: &str) -> Result<usize, AppError> {
        let result = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name("OwnerIndex")
            .key_condition_expression("OwnerId = :oid")
            .expression_attribute_values(":oid", AttributeValue::S(owner_id.to_string()))
            .select(aws_sdk_dynamodb::types::Select::Count)
            .send()
            .await
            .map_err(AppError::database)?;

        Ok(result.count() as usize)
    }

    /// Store a newly minted key.
    async fn put_key(
        &self,
        key_hash: &str,
        owner_id: &str,
        label: &str,
        key_prefix: &str,
        created_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), AppError> {
        let mut item = vec![
            ("KeyHash".to_string(), AttributeValue::S(key_hash.to_string())),
            ("OwnerId".to_string(), AttributeValue::S(owner_id.to_string())),
            ("Label".to_string(), AttributeValue::S(label.to_string())),
            ("KeyPrefix".to_string(), AttributeValue::S(key_prefix.to_string())),
            ("CreatedAt".to_string(), AttributeValue::N(created_at.to_string())),
        ];

        if let Some(exp) = expires_at {
            item.push(("ExpiresAt".to_string(), AttributeValue::N(exp.to_string())));
        }

        let mut put = self
            .client
            .put_item()
            .table_name(&self.table_name);

        for (k, v) in item {
            put = put.item(k, v);
        }

        put.send().await.map_err(AppError::database)?;
        Ok(())
    }

    /// List all keys for an owner (newest first).
    async fn list_keys(&self, owner_id: &str) -> Result<Vec<KeySummary>, AppError> {
        let result = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name("OwnerIndex")
            .key_condition_expression("OwnerId = :oid")
            .expression_attribute_values(":oid", AttributeValue::S(owner_id.to_string()))
            .scan_index_forward(false)
            .send()
            .await
            .map_err(AppError::database)?;

        let keys = result
            .items()
            .iter()
            .map(|item| {
                let key_id = item
                    .get("KeyHash")
                    .and_then(|v| v.as_s().ok())
                    .unwrap_or(&String::new())
                    .clone();
                let prefix = item
                    .get("KeyPrefix")
                    .and_then(|v| v.as_s().ok())
                    .unwrap_or(&String::new())
                    .clone();
                let label = item
                    .get("Label")
                    .and_then(|v| v.as_s().ok())
                    .unwrap_or(&String::new())
                    .clone();
                let created_at = item
                    .get("CreatedAt")
                    .and_then(|v| v.as_n().ok())
                    .and_then(|n| n.parse::<i64>().ok())
                    .unwrap_or(0);
                let last_used_at = item
                    .get("LastUsedAt")
                    .and_then(|v| v.as_n().ok())
                    .and_then(|n| n.parse::<i64>().ok());
                let expires_at = item
                    .get("ExpiresAt")
                    .and_then(|v| v.as_n().ok())
                    .and_then(|n| n.parse::<i64>().ok());

                KeySummary {
                    key_id,
                    prefix,
                    label,
                    created_at,
                    last_used_at,
                    expires_at,
                }
            })
            .collect();

        Ok(keys)
    }

    /// Get a key item by hash, returning the OwnerId if it exists.
    async fn get_key_owner(&self, key_hash: &str) -> Result<Option<String>, AppError> {
        let result = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("KeyHash", AttributeValue::S(key_hash.to_string()))
            .projection_expression("OwnerId")
            .send()
            .await
            .map_err(AppError::database)?;

        Ok(result
            .item()
            .and_then(|item| item.get("OwnerId"))
            .and_then(|v| v.as_s().ok())
            .map(|s| s.to_string()))
    }

    /// Delete a key by hash.
    async fn delete_key(&self, key_hash: &str) -> Result<(), AppError> {
        self.client
            .delete_item()
            .table_name(&self.table_name)
            .key("KeyHash", AttributeValue::S(key_hash.to_string()))
            .send()
            .await
            .map_err(AppError::database)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

async fn handle_mint(
    store: &KeyStore,
    owner_id: &str,
    body: &str,
) -> Result<lambda_http::Response<lambda_http::Body>, Error> {
    // Parse request body
    let req: MintRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => {
            return error_response(&AppError::Validation(
                "Invalid request body: expected JSON with 'label' field".into(),
            ));
        }
    };

    // Validate label
    if req.label.trim().is_empty() {
        return error_response(&AppError::Validation("Label must not be empty".into()));
    }

    // Validate expires_in_days and convert to an absolute timestamp in one place, so the
    // rule is reachable from a test rather than buried in the request path.
    let now = Utc::now().timestamp();
    let expires_at = match expiry_from_days(req.expires_in_days, now) {
        Ok(expiry) => expiry,
        Err(e) => return error_response(&e),
    };

    // Enforce 10-key cap
    let count = store.count_owner_keys(owner_id).await?;
    if count >= MAX_KEYS_PER_OWNER {
        return error_response(&AppError::Validation(format!(
            "Maximum of {MAX_KEYS_PER_OWNER} API keys reached"
        )));
    }

    // Generate key
    let plaintext = generate_key();
    let key_hash = hash_key(&plaintext);
    let prefix = key_prefix(&plaintext);

    // Store in DynamoDB
    store
        .put_key(&key_hash, owner_id, &req.label, &prefix, now, expires_at)
        .await?;

    let response = MintResponse {
        key: plaintext,
        key_id: key_hash,
        prefix,
        label: req.label,
        created_at: now,
        expires_at,
    };

    json_response(&StatusCode::CREATED, &response)
}

async fn handle_list(store: &KeyStore, owner_id: &str) -> Result<lambda_http::Response<lambda_http::Body>, Error> {
    let keys = store.list_keys(owner_id).await?;
    let response = ListResponse { keys };
    json_response(&StatusCode::OK, &response)
}

async fn handle_revoke(
    store: &KeyStore,
    owner_id: &str,
    key_id: &str,
) -> Result<lambda_http::Response<lambda_http::Body>, Error> {
    // Verify the key exists AND belongs to this owner.
    // If not found or owner mismatch, return 403 — never reveal existence.
    let stored_owner = store.get_key_owner(key_id).await?;

    match stored_owner {
        Some(ref owner) if owner == owner_id => {
            store.delete_key(key_id).await?;
            empty_response(&StatusCode::NO_CONTENT)
        }
        _ => error_response(&AppError::Forbidden),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Which handler a request resolves to.
///
/// Split out from `function_handler` so the routing decision can be tested without a
/// DynamoDB client. The bug this replaced returned 405 for every single request, and
/// no test could see it because the only routing code sat behind live AWS calls.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Mint,
    List,
    /// Carries the key id, which may be empty -- the handler rejects that as a 400
    /// rather than a 405, since the caller clearly meant to revoke something.
    Revoke(String),
    NotAllowed,
}

fn route_of(method: &str, path: &str) -> Route {
    match (method, path) {
        ("POST", "/api/keys") => Route::Mint,
        ("GET", "/api/keys") => Route::List,
        ("DELETE", p) if p.starts_with("/api/keys/") => {
            Route::Revoke(p["/api/keys/".len()..].to_string())
        }
        _ => Route::NotAllowed,
    }
}

/// The path to route on: the request path with the API Gateway stage prefix removed.
///
/// Neither obvious accessor gives this directly. On a named stage (this API uses `prod`)
/// API Gateway sends `rawPath` *including* the stage, so `raw_http_path()` returns
/// `/prod/api/keys`; and `event.uri().path()` is built by lambda_http's
/// `apigw_path_with_stage`, which keeps that prefix too. Matching either against a
/// literal `/api/keys` therefore failed for mint, list AND revoke, and every request
/// fell through to the 405 arm -- which reads as a method problem rather than a path one.
/// Upstream pins this behaviour in `deserializes_apigw_http_request_with_stage_in_path`.
///
/// So strip the stage using the value the request itself carries, rather than trusting
/// either accessor to have done it.
///
/// The other Lambdas in this stack are immune only because each owns a single route and
/// never inspects the path.
fn path_for_routing(event: &Request) -> String {
    let raw = event.raw_http_path();
    // A direct invoke with no rawPath leaves the extension unset; fall back to the URI.
    let path = if raw.is_empty() {
        event.uri().path()
    } else {
        raw
    };
    strip_stage_prefix(path, stage_of(event).as_deref())
}

fn stage_of(event: &Request) -> Option<String> {
    match event.request_context_ref() {
        Some(RequestContext::ApiGatewayV2(ctx)) => ctx.stage.clone(),
        _ => None,
    }
}

/// Removes a leading `/<stage>` segment. `$default` is never present in the path.
fn strip_stage_prefix(path: &str, stage: Option<&str>) -> String {
    let stage = match stage {
        Some(s) if !s.is_empty() && s != "$default" => s,
        _ => return path.to_string(),
    };

    match path.strip_prefix(&format!("/{stage}")) {
        // Require a following '/' so a stage named `prod` cannot eat the first segment
        // of an unrelated path like `/production/thing`.
        Some(rest) if rest.starts_with('/') => rest.to_string(),
        Some("") => "/".to_string(),
        _ => path.to_string(),
    }
}

async fn function_handler(
    store: &KeyStore,
    event: Request,
) -> Result<lambda_http::Response<lambda_http::Body>, Error> {
    tracing::info!("Received event: {:?}", event);

    let owner_id = match owner_from_request(&event) {
        Ok(sub) => sub,
        Err(e) => {
            tracing::error!("rejecting request without owner identity: {:?}", e);
            return error_response(&e);
        }
    };

    let path = path_for_routing(&event);

    match route_of(event.method().as_str(), &path) {
        Route::Mint => {
            let body = match event.body() {
                lambda_http::Body::Text(s) => s.clone(),
                lambda_http::Body::Binary(b) => {
                    String::from_utf8(b.clone()).unwrap_or_default()
                }
                lambda_http::Body::Empty => String::new(),
                // `Body` is #[non_exhaustive], so a wildcard is required.
                _ => String::new(),
            };
            handle_mint(store, &owner_id, &body).await
        }
        Route::List => handle_list(store, &owner_id).await,
        Route::Revoke(key_id) => {
            if key_id.is_empty() {
                return error_response(&AppError::Validation("Key ID is required".into()));
            }
            handle_revoke(store, &owner_id, &key_id).await
        }
        Route::NotAllowed => {
            tracing::warn!("no route for {} {}", event.method(), path);
            empty_response(&StatusCode::METHOD_NOT_ALLOWED)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();

    let table_name =
        env::var("API_KEY_TABLE_NAME").expect("No API_KEY_TABLE_NAME environment variable set");

    let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12()).load().await;
    let dynamodb_client = aws_sdk_dynamodb::Client::new(&config);

    let store = KeyStore::new(&table_name, dynamodb_client);

    run(service_fn(|event| function_handler(&store, event))).await
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_format() {
        let key = generate_key();
        assert!(key.starts_with("krtk_"), "key should start with krtk_");
        assert_eq!(key.len(), 48, "key should be 48 chars (5 prefix + 43 random)");
    }

    #[test]
    fn generated_key_is_unique() {
        let k1 = generate_key();
        let k2 = generate_key();
        assert_ne!(k1, k2, "two generated keys should differ");
    }

    #[test]
    fn prefix_extraction() {
        let key = "krtk_3f9aQ2xABCDEFGHIJKLMNOPQRSTUVWXYZ01234567";
        let prefix = key_prefix(key);
        assert_eq!(prefix, "krtk_3f9aQ2x");
        assert_eq!(prefix.len(), KEY_PREFIX_LEN);
    }

    #[test]
    fn hash_determinism() {
        let key = "krtk_abc123def456ghi789jkl012mno345pqr678stu90v";
        let h1 = hash_key(key);
        let h2 = hash_key(key);
        assert_eq!(h1, h2, "hashing the same key must produce the same output");
        // SHA-256 hex is always 64 chars
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn hash_differs_for_different_keys() {
        let h1 = hash_key("krtk_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let h2 = hash_key("krtk_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(h1, h2);
    }

    #[test]
    fn no_expiry_means_a_key_that_never_expires() {
        assert_eq!(expiry_from_days(None, 1_000).unwrap(), None);
    }

    #[test]
    fn zero_days_is_rejected() {
        // Otherwise a caller passing 0 would mint a key that is already expired, which
        // reads as "the key I just created does not work".
        assert!(matches!(
            expiry_from_days(Some(0), 1_000),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn expiry_is_bounded_at_the_maximum() {
        assert!(expiry_from_days(Some(MAX_EXPIRY_DAYS), 1_000).is_ok());
        assert!(matches!(
            expiry_from_days(Some(MAX_EXPIRY_DAYS + 1), 1_000),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn expiry_is_computed_forward_from_now() {
        // One day past the epoch-ish baseline, not an absolute constant, so the maths is
        // actually checked rather than the constant restated.
        assert_eq!(expiry_from_days(Some(1), 1_000).unwrap(), Some(1_000 + 86_400));
        assert_eq!(expiry_from_days(Some(30), 0).unwrap(), Some(30 * 86_400));
    }

    #[test]
    fn key_cap_constant() {
        assert_eq!(MAX_KEYS_PER_OWNER, 10);
    }

    #[test]
    fn base64url_no_padding_in_key() {
        let key = generate_key();
        let random_part = &key[KEY_PREFIX.len()..];
        assert!(
            !random_part.contains('='),
            "base64url should have no padding"
        );
        assert!(
            !random_part.contains('+'),
            "base64url should not contain +"
        );
        assert!(
            !random_part.contains('/'),
            "base64url should not contain /"
        );
    }

    // -----------------------------------------------------------------------
    // Routing
    // -----------------------------------------------------------------------

    /// A real HTTP API (payload 2.0) event on the `prod` stage, which is what this
    /// function is actually deployed behind.
    ///
    /// `rawPath` here deliberately CARRIES the stage prefix, because that is what API
    /// Gateway sends for a named stage -- upstream pins the same shape in
    /// `apigw_v2_proxy_request_with_stage_in_path.json`. An earlier version of this
    /// fixture passed a stage-free path, which made the routing test pass while
    /// production still returned 405 for every request. The fixture has to look the way
    /// production actually looks or it proves nothing.
    fn staged_event(method: &str, path_after_stage: &str, body: &str) -> Request {
        let raw_path = format!("/prod{path_after_stage}");
        let json = format!(
            r#"{{
              "version": "2.0",
              "routeKey": "{method} /api/keys",
              "rawPath": "{raw_path}",
              "rawQueryString": "",
              "headers": {{ "content-type": "application/json" }},
              "body": {body:?},
              "isBase64Encoded": false,
              "requestContext": {{
                "accountId": "123456789012",
                "apiId": "api-id",
                "authorizer": {{ "jwt": {{ "claims": {{ "sub": "owner-sub" }}, "scopes": null }} }},
                "domainName": "api-id.execute-api.us-west-2.amazonaws.com",
                "domainPrefix": "api-id",
                "http": {{
                  "method": "{method}",
                  "path": "{raw_path}",
                  "protocol": "HTTP/1.1",
                  "sourceIp": "1.2.3.4",
                  "userAgent": "test"
                }},
                "requestId": "id",
                "routeKey": "{method} /api/keys",
                "stage": "prod",
                "time": "15/Aug/2026:03:00:00 +0000",
                "timeEpoch": 1786000000000
              }}
            }}"#
        );
        lambda_http::request::from_str(&json).expect("fixture should deserialize")
    }

    /// The regression test for the 405-on-everything bug.
    ///
    /// Both accessors keep the stage prefix on a named stage, so matching either against
    /// a literal `/api/keys` compared `/prod/api/keys` and fell through to the 405 arm
    /// for mint, list and revoke alike. The first two assertions pin that trap, so this
    /// test explains the failure rather than merely detecting it.
    #[test]
    fn routes_on_the_stage_stripped_path() {
        let event = staged_event("POST", "/api/keys", r#"{"label":"CLI"}"#);

        assert_eq!(
            event.uri().path(),
            "/prod/api/keys",
            "the constructed URI still carries the stage"
        );
        assert_eq!(
            event.raw_http_path(),
            "/prod/api/keys",
            "raw_http_path does NOT strip the stage -- this is the trap"
        );
        assert_eq!(path_for_routing(&event), "/api/keys");
        assert_eq!(
            route_of(event.method().as_str(), &path_for_routing(&event)),
            Route::Mint
        );
    }

    #[test]
    fn stage_stripping_is_conservative() {
        // The normal case.
        assert_eq!(strip_stage_prefix("/prod/api/keys", Some("prod")), "/api/keys");
        // A path that merely starts with the stage name must be left alone.
        assert_eq!(
            strip_stage_prefix("/production/api/keys", Some("prod")),
            "/production/api/keys"
        );
        // $default is never present in the path.
        assert_eq!(
            strip_stage_prefix("/api/keys", Some("$default")),
            "/api/keys"
        );
        assert_eq!(strip_stage_prefix("/api/keys", None), "/api/keys");
        // Already-stripped input is idempotent, so a future lambda_http that strips the
        // stage itself would not break routing.
        assert_eq!(strip_stage_prefix("/api/keys", Some("prod")), "/api/keys");
        // The stage alone.
        assert_eq!(strip_stage_prefix("/prod", Some("prod")), "/");
    }

    #[test]
    fn staged_list_and_revoke_also_route() {
        let list = staged_event("GET", "/api/keys", "");
        assert_eq!(
            route_of(list.method().as_str(), &path_for_routing(&list)),
            Route::List
        );

        let revoke = staged_event("DELETE", "/api/keys/abc123", "");
        assert_eq!(
            route_of(revoke.method().as_str(), &path_for_routing(&revoke)),
            Route::Revoke("abc123".to_string())
        );
    }

    /// Mint with no expiry is the case that surfaced the bug: it is valid input, so it
    /// must reach the mint handler rather than being rejected at the router.
    #[test]
    fn mint_without_an_expiry_is_a_routing_match_not_a_405() {
        let event = staged_event("POST", "/api/keys", r#"{"label":"Midzor CLI"}"#);
        assert_eq!(
            route_of(event.method().as_str(), &path_for_routing(&event)),
            Route::Mint
        );
        // ...and the absent expiry is accepted by the validation it then reaches.
        assert_eq!(expiry_from_days(None, 1_000).unwrap(), None);
    }

    #[test]
    fn unknown_method_or_path_is_a_405() {
        assert_eq!(route_of("PUT", "/api/keys"), Route::NotAllowed);
        assert_eq!(route_of("GET", "/api/links"), Route::NotAllowed);
        assert_eq!(route_of("DELETE", "/api/keys"), Route::NotAllowed);
    }

    /// An empty key id is a malformed revoke, not a wrong method -- the handler answers
    /// 400. Routing it to NotAllowed would report the wrong problem.
    #[test]
    fn revoke_with_no_key_id_still_routes_to_revoke() {
        assert_eq!(
            route_of("DELETE", "/api/keys/"),
            Route::Revoke(String::new())
        );
    }
}
