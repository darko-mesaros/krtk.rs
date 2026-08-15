use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::jwks::JwksCache;

#[derive(Debug, Deserialize)]
pub struct CognitoClaims {
    pub sub: String,
    /// Issuer. Validated by `jsonwebtoken` via `Validation::set_issuer`, not by our own
    /// code — kept on the struct because it is worth having in a log line when a token
    /// is rejected, and because dropping it would silently narrow what we deserialize.
    #[allow(dead_code)]
    pub iss: String,
    pub token_use: String,
    pub client_id: String,
    /// Expiry. Validated by `jsonwebtoken` via `validate_exp`; see `iss` above.
    #[allow(dead_code)]
    pub exp: u64,
}

/// Checks the claims that Cognito puts in custom fields, which `jsonwebtoken` cannot
/// validate for us.
///
/// Split out from `validate_token` so it can be tested without minting and signing a
/// real JWT: signature verification is the library's job and is not what this logic
/// gets wrong. The two failure modes here are the ones that matter.
fn check_cognito_claims(
    claims: &CognitoClaims,
    expected_client_id: &str,
) -> Result<(), JwtError> {
    // An ID token is NOT an API credential. It is audience-scoped to the app client and
    // carries identity claims, not authorization, yet it is signed by the same pool and
    // will pass every signature check. Accepting one would work perfectly in testing and
    // be wrong in production, so this check is the point of the function.
    if claims.token_use != "access" {
        return Err(JwtError::InvalidTokenUse(claims.token_use.clone()));
    }

    // A token minted for a different app client in the same pool is signed by the same
    // keys, so the signature proves nothing about which client it was issued to.
    if claims.client_id != expected_client_id {
        return Err(JwtError::InvalidToken("Client ID mismatch".to_string()));
    }

    Ok(())
}

/// Validate a JWT against the Cognito JWKS.
///
/// Checks:
/// - RS256 signature against the matching kid from JWKS
/// - `iss` matches the expected pool issuer
/// - `token_use` is "access" (rejects ID tokens)
/// - `client_id` matches the expected app client
/// - `exp` has not passed (handled by jsonwebtoken)
pub async fn validate_token(
    token: &str,
    jwks_cache: &JwksCache,
    expected_issuer: &str,
    expected_client_id: &str,
) -> Result<CognitoClaims, JwtError> {
    // Decode header to get kid
    let header = decode_header(token).map_err(|e| JwtError::InvalidToken(e.to_string()))?;

    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| JwtError::InvalidToken("Missing kid in token header".to_string()))?;

    // Get JWKS (with rotation-aware refresh)
    let jwks = jwks_cache
        .get_jwks_for_kid(kid)
        .await
        .map_err(|e| JwtError::KeyFetch(e.to_string()))?;

    // Find the matching key
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.common.key_id.as_deref() == Some(kid))
        .ok_or_else(|| JwtError::InvalidToken(format!("No key found for kid '{kid}'")))?;

    let decoding_key =
        DecodingKey::from_jwk(jwk).map_err(|e| JwtError::InvalidToken(e.to_string()))?;

    // Set up validation
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[expected_issuer]);
    validation.validate_exp = true;
    // We check client_id manually since Cognito puts it in a custom claim
    validation.set_audience::<String>(&[]);

    let token_data = decode::<CognitoClaims>(token, &decoding_key, &validation)
        .map_err(|e| JwtError::InvalidToken(e.to_string()))?;

    let claims = token_data.claims;

    check_cognito_claims(&claims, expected_client_id)?;

    Ok(claims)
}

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("Token verification failed")]
    InvalidToken(String),

    #[error("Token verification failed")]
    KeyFetch(String),

    #[error("Invalid token_use: expected 'access', got '{0}'")]
    InvalidTokenUse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = "test-client";

    fn claims(token_use: &str, client_id: &str) -> CognitoClaims {
        CognitoClaims {
            sub: "user-123".to_string(),
            iss: "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_abc123".to_string(),
            token_use: token_use.to_string(),
            client_id: client_id.to_string(),
            exp: 9_999_999_999,
        }
    }

    #[test]
    fn accepts_an_access_token_from_the_expected_client() {
        assert!(check_cognito_claims(&claims("access", CLIENT), CLIENT).is_ok());
    }

    /// The security-critical case. An ID token is signed by the same pool with the same
    /// keys, so it passes every signature check — only this claim distinguishes it.
    #[test]
    fn rejects_an_id_token() {
        let err = check_cognito_claims(&claims("id", CLIENT), CLIENT)
            .expect_err("an ID token must not authenticate an API call");
        assert!(matches!(err, JwtError::InvalidTokenUse(ref u) if u == "id"));
    }

    #[test]
    fn rejects_an_unknown_token_use() {
        // Fail closed on anything unrecognised rather than allow-listing by exclusion.
        assert!(check_cognito_claims(&claims("refresh", CLIENT), CLIENT).is_err());
        assert!(check_cognito_claims(&claims("", CLIENT), CLIENT).is_err());
    }

    /// A token minted for a different app client in the same pool carries a valid
    /// signature, so the client_id check is the only thing rejecting it.
    #[test]
    fn rejects_a_token_from_another_client_in_the_same_pool() {
        let err = check_cognito_claims(&claims("access", "some-other-client"), CLIENT)
            .expect_err("a token for another client must be rejected");
        assert!(matches!(err, JwtError::InvalidToken(_)));
    }

    /// token_use is checked before client_id: a wrong-type token should report as such
    /// rather than as a client mismatch, so logs point at the real problem.
    #[test]
    fn token_use_is_checked_before_client_id() {
        let err = check_cognito_claims(&claims("id", "some-other-client"), CLIENT).unwrap_err();
        assert!(matches!(err, JwtError::InvalidTokenUse(_)));
    }

    /// The Display text reaching a caller must not describe why verification failed.
    #[test]
    fn error_messages_do_not_explain_the_failure() {
        assert_eq!(
            JwtError::InvalidToken("client id mismatch on kid abc".into()).to_string(),
            "Token verification failed"
        );
        assert_eq!(
            JwtError::KeyFetch("jwks 503 from cognito-idp".into()).to_string(),
            "Token verification failed"
        );
    }
}
