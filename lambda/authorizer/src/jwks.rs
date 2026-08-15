use jsonwebtoken::jwk::JwkSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour

#[derive(Debug)]
struct CacheEntry {
    jwks: JwkSet,
    fetched_at: Instant,
}

#[derive(Debug, Clone)]
pub struct JwksCache {
    url: String,
    cache: Arc<RwLock<Option<CacheEntry>>>,
    client: reqwest::Client,
}

impl JwksCache {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            cache: Arc::new(RwLock::new(None)),
            client: reqwest::Client::new(),
        }
    }

    /// Get the cached JWKS, refreshing if stale or missing.
    pub async fn get_jwks(&self) -> Result<JwkSet, JwksError> {
        // Check if cache is valid
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.as_ref()
                && entry.fetched_at.elapsed() < CACHE_TTL
            {
                return Ok(entry.jwks.clone());
            }
        }

        // Cache miss or expired — fetch fresh
        self.refresh().await
    }

    /// Force refresh and return the new JWKS (used on unknown kid).
    pub async fn refresh(&self) -> Result<JwkSet, JwksError> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|e| JwksError::Fetch(e.to_string()))?;

        if !response.status().is_success() {
            return Err(JwksError::Fetch(format!(
                "JWKS endpoint returned {}",
                response.status()
            )));
        }

        let jwks: JwkSet = response
            .json()
            .await
            .map_err(|e| JwksError::Parse(e.to_string()))?;

        let mut cache = self.cache.write().await;
        *cache = Some(CacheEntry {
            jwks: jwks.clone(),
            fetched_at: Instant::now(),
        });

        Ok(jwks)
    }

    /// Get JWKS, re-fetching if the given kid is not found in cache.
    pub async fn get_jwks_for_kid(&self, kid: &str) -> Result<JwkSet, JwksError> {
        let jwks = self.get_jwks().await?;

        // Check if kid exists in current set
        let has_kid = jwks.keys.iter().any(|k| {
            k.common.key_id.as_deref() == Some(kid)
        });

        if has_kid {
            Ok(jwks)
        } else {
            // Key rotation: force refresh
            tracing::info!("Unknown kid '{kid}', refreshing JWKS");
            self.refresh().await
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    #[error("Failed to fetch key set")]
    Fetch(String),

    #[error("Failed to parse key set")]
    Parse(String),
}
