//! Resolving the calling owner from a verified request.
//!
//! # Why this exists
//!
//! `/api/links` and `/api/keys` are protected by **two different** API Gateway
//! authorizers, and each puts the caller's identity in a different place:
//!
//! | Route         | Authorizer              | Owner location                             |
//! |---------------|-------------------------|--------------------------------------------|
//! | `/api/links`  | custom Lambda (REQUEST) | `authorizer.lambda.ownerId` (we set it)    |
//! | `/api/keys`   | native user pool (JWT)  | `authorizer.jwt.claims.sub` (Cognito sets) |
//!
//! `/api/links` needs the custom authorizer because it accepts either a Cognito
//! JWT or an API key (FR-4.3). `/api/keys` deliberately uses the *native* pool
//! authorizer so an API key cannot reach key management at all (FR-4.4) — the
//! boundary is enforced at the edge rather than by a conditional in a handler.
//!
//! The cost of that choice is these two shapes. Code written against only the
//! Lambda shape compiles cleanly and fails at runtime on **every** `/api/keys`
//! request and nowhere else, so a test suite exercising links but not keys stays
//! green. Both shapes are therefore handled here, once, with tests covering both
//! plus the absent case.
//!
//! Handlers must call this rather than reaching into the request context.
//!
//! Note on the field name: in `aws_lambda_events` the custom-authorizer context
//! map is the Rust field `fields`, serde-renamed from the wire name `lambda`.

use lambda_http::aws_lambda_events::apigw::ApiGatewayRequestAuthorizer;
use lambda_http::request::RequestContext;
use lambda_http::{tracing, Request, RequestExt};

use crate::error::AppError;

/// Extracts the authenticated owner's Cognito `sub` from a request.
///
/// Tries the custom Lambda authorizer's context first, then the native JWT
/// claims. Returns [`AppError::Unauthorized`] when neither is present, which
/// should be unreachable behind a configured authorizer — a request reaching a
/// handler without identity means the route is misconfigured, and failing closed
/// is the only safe response.
///
/// The value is never read from a header, query parameter, or body: those are
/// caller-controlled, and trusting them would let anyone claim any owner (FR-3.2).
pub fn owner_from_request(event: &Request) -> Result<String, AppError> {
    let authorizer = match event.request_context_ref() {
        Some(RequestContext::ApiGatewayV2(ctx)) => ctx.authorizer.as_ref(),
        // The API is an HTTP API (v2). Any other context shape means the function was
        // invoked through a path this design does not use, so there is no trustworthy
        // identity to read.
        _ => None,
    };

    if let Some(auth) = authorizer {
        if let Some(owner) = owner_from_lambda_context(auth) {
            return Ok(owner);
        }
        if let Some(owner) = owner_from_jwt_claims(auth) {
            return Ok(owner);
        }
    }

    tracing::error!(
        "no owner identity in request context -- route is missing an authorizer, or the \
         authorizer did not populate ownerId/sub"
    );
    Err(AppError::Unauthorized)
}

/// Custom Lambda authorizer (`/api/links`): reads the `ownerId` we set ourselves.
fn owner_from_lambda_context(auth: &ApiGatewayRequestAuthorizer) -> Option<String> {
    auth.fields
        .get("ownerId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Native user pool authorizer (`/api/keys`): reads the `sub` claim Cognito supplies.
fn owner_from_jwt_claims(auth: &ApiGatewayRequestAuthorizer) -> Option<String> {
    auth.jwt
        .as_ref()
        .and_then(|jwt| jwt.claims.get("sub"))
        .filter(|s| !s.is_empty())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lambda_http::aws_lambda_events::apigw::{
        ApiGatewayRequestAuthorizerJwtDescription, ApiGatewayV2httpRequestContext,
    };

    // The event structs are #[non_exhaustive], so they are built via Default and
    // field assignment rather than struct literals.

    fn request_with_context(authorizer: Option<ApiGatewayRequestAuthorizer>) -> Request {
        let mut ctx = ApiGatewayV2httpRequestContext::default();
        ctx.authorizer = authorizer;

        let mut req = Request::default();
        req.extensions_mut()
            .insert(RequestContext::ApiGatewayV2(ctx));
        req
    }

    /// The shape the CUSTOM Lambda authorizer produces.
    fn lambda_authorizer(owner: &str) -> ApiGatewayRequestAuthorizer {
        let mut auth = ApiGatewayRequestAuthorizer::default();
        auth.fields
            .insert("ownerId".to_string(), serde_json::json!(owner));
        auth
    }

    /// The shape the NATIVE user pool authorizer produces.
    fn jwt_authorizer(sub: &str) -> ApiGatewayRequestAuthorizer {
        let mut jwt = ApiGatewayRequestAuthorizerJwtDescription::default();
        jwt.claims.insert("sub".to_string(), sub.to_string());

        let mut auth = ApiGatewayRequestAuthorizer::default();
        auth.jwt = Some(jwt);
        auth
    }

    #[test]
    fn reads_owner_from_custom_lambda_authorizer() {
        let req = request_with_context(Some(lambda_authorizer("sub-from-lambda")));
        assert_eq!(owner_from_request(&req).unwrap(), "sub-from-lambda");
    }

    /// The case a links-only test suite would miss entirely, because it only ever
    /// occurs on /api/keys.
    #[test]
    fn reads_owner_from_native_jwt_claims() {
        let req = request_with_context(Some(jwt_authorizer("sub-from-jwt")));
        assert_eq!(owner_from_request(&req).unwrap(), "sub-from-jwt");
    }

    #[test]
    fn rejects_request_with_no_authorizer_context() {
        let req = Request::default();
        assert!(matches!(
            owner_from_request(&req),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_context_with_authorizer_but_no_identity() {
        let req = request_with_context(Some(ApiGatewayRequestAuthorizer::default()));
        assert!(matches!(
            owner_from_request(&req),
            Err(AppError::Unauthorized)
        ));
    }

    /// An authorizer that ran but populated an empty value must fail closed rather
    /// than yielding "" as an owner, which would partition links under `USER#`.
    #[test]
    fn rejects_empty_owner_value() {
        let req = request_with_context(Some(lambda_authorizer("")));
        assert!(matches!(
            owner_from_request(&req),
            Err(AppError::Unauthorized)
        ));
    }
}
