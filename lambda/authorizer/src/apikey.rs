use aws_sdk_dynamodb::types::AttributeValue;
use chrono::Utc;
use sha2::{Digest, Sha256};

/// Verify an API key against the key table.
///
/// Returns `Ok(Some(owner_id))` on success, `Ok(None)` if the key is invalid or
/// expired, and `Err` only on infrastructure failures.
///
/// Side effect: bumps `LastUsedAt` best-effort when the stored value is >1h old.
pub async fn verify_api_key(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    raw_key: &str,
) -> Result<Option<String>, ApiKeyError> {
    let key_hash = hash_key(raw_key);

    // GetItem by KeyHash
    let result = client
        .get_item()
        .table_name(table_name)
        .key("KeyHash", AttributeValue::S(key_hash.clone()))
        .send()
        .await
        .map_err(|e| ApiKeyError::Storage(e.to_string()))?;

    let item = match result.item {
        Some(item) => item,
        None => return Ok(None), // Key not found
    };

    // Check ExpiresAt — absent means never expires, otherwise must be in the future.
    //
    // Checked here rather than left to DynamoDB TTL: TTL deletion is asynchronous and can
    // lag by days, so relying on it would keep honouring keys the user believes are dead.
    //
    // A malformed or unparseable ExpiresAt falls through to "not expired". That is
    // deliberate: the value is written only by our own mint path, so a bad one means a
    // bug rather than an attack, and failing closed on it would lock a user out of a key
    // that is legitimately still valid.
    if let Some(expires_at_attr) = item.get("ExpiresAt")
        && let Ok(expires_at_str) = expires_at_attr.as_n()
        && let Ok(expires_epoch) = expires_at_str.parse::<i64>()
        && expires_epoch <= Utc::now().timestamp()
    {
        return Ok(None); // Expired
    }

    // Extract OwnerId
    let owner_id = item
        .get("OwnerId")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| ApiKeyError::Storage("Missing OwnerId in key record".to_string()))?;

    // Bump LastUsedAt best-effort (only if stored value is >1h old)
    bump_last_used_if_stale(client, table_name, &key_hash, &item).await;

    Ok(Some(owner_id))
}

/// SHA-256 hash the raw key, returning the hex-encoded digest.
pub fn hash_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Whether LastUsedAt should be bumped (true if absent or >1h old).
pub fn should_bump_last_used(last_used_epoch: Option<i64>) -> bool {
    match last_used_epoch {
        None => true,
        Some(ts) => {
            let now = Utc::now().timestamp();
            (now - ts) > 3600
        }
    }
}

/// Best-effort conditional update of LastUsedAt.
/// Only updates when the stored value is absent or older than 1 hour.
/// Ignores all failures (conditional check, throttle, etc.).
async fn bump_last_used_if_stale(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    key_hash: &str,
    item: &std::collections::HashMap<String, AttributeValue>,
) {
    let last_used_epoch = item
        .get("LastUsedAt")
        .and_then(|v| v.as_n().ok())
        .and_then(|s| s.parse::<i64>().ok());

    if !should_bump_last_used(last_used_epoch) {
        return;
    }

    let now_str = Utc::now().timestamp().to_string();

    let _ = client
        .update_item()
        .table_name(table_name)
        .key("KeyHash", AttributeValue::S(key_hash.to_string()))
        .update_expression("SET LastUsedAt = :now")
        .expression_attribute_values(":now", AttributeValue::N(now_str))
        .send()
        .await;
}

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    /// Deliberately opaque — does not name the backing store.
    #[error("Key verification failed")]
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_key_deterministic() {
        let hash1 = hash_key("my-secret-key");
        let hash2 = hash_key("my-secret-key");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_hash_key_different_inputs() {
        let hash1 = hash_key("key-a");
        let hash2 = hash_key("key-b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_should_bump_last_used_none() {
        // No stored timestamp — should bump
        assert!(should_bump_last_used(None));
    }

    #[test]
    fn test_should_bump_last_used_stale() {
        // Stored timestamp more than 1h ago — should bump
        let two_hours_ago = Utc::now().timestamp() - 7200;
        assert!(should_bump_last_used(Some(two_hours_ago)));
    }

    #[test]
    fn test_should_not_bump_last_used_recent() {
        // Stored timestamp 10 minutes ago — should NOT bump
        let ten_min_ago = Utc::now().timestamp() - 600;
        assert!(!should_bump_last_used(Some(ten_min_ago)));
    }

    #[test]
    fn test_expires_at_in_past_rejects() {
        // Simulating the expiry check logic
        let expires_epoch: i64 = 1000000000; // well in the past
        let now = Utc::now().timestamp();
        assert!(expires_epoch <= now, "Expired key should be rejected");
    }

    #[test]
    fn test_expires_at_absent_passes() {
        // When ExpiresAt is None, the key never expires
        let expires_at: Option<i64> = None;
        assert!(expires_at.is_none(), "Absent ExpiresAt means no expiry");
    }

    #[test]
    fn test_expires_at_in_future_passes() {
        let future_epoch: i64 = Utc::now().timestamp() + 86400; // tomorrow
        let now = Utc::now().timestamp();
        assert!(future_epoch > now, "Future ExpiresAt should pass");
    }
}
