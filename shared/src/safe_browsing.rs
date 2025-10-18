use serde::{Deserialize, Serialize};
use aws_sdk_secretsmanager::Client as SecretsClient;

#[derive(Serialize, Debug)]
struct SafeBrowsingRequest {
    client: ClientInfo,
    #[serde(rename="threatInfo")]
    threat_info: ThreatInfo
}

#[derive(Serialize, Debug)]
struct ClientInfo {
    #[serde(rename="clientId")]
    client_id: String,
    #[serde(rename="clientVersion")]
    client_version: String,
}

#[derive(Serialize, Debug)]
struct ThreatInfo{
    #[serde(rename="threatTypes")]
    threat_types: Vec<String>,
    #[serde(rename="platformTypes")]
    platform_types: Vec<String>,
    #[serde(rename="threatEntryTypes")]
    threat_entry_types: Vec<String>,
    #[serde(rename="threatEntries")]
    threat_entries: Vec<ThreatEntry>
}

#[derive(Serialize, Debug)]
struct ThreatEntry {
    url: String,
}

#[derive(Deserialize, Debug)]
struct SafeBrowsingResponse {
    matches: Option<Vec<serde_json::Value>>

}

pub async fn is_url_safe(url: &str, secrets_client: &SecretsClient, secret_arn: &str, http_client: &reqwest::Client) -> Result<bool, String> {
    let api_key = get_api_key(secrets_client, secret_arn).await?;

    let request = SafeBrowsingRequest {
        client: ClientInfo { 
            client_id: "krtkt-rs".to_string(),
            client_version: "0.0.5".to_string()
        },
        threat_info: ThreatInfo { 
            threat_types: vec!["MALWARE".to_string(), "SOCIAL_ENGINEERING".to_string()],
            platform_types: vec!["ANY_PLATFORM".to_string()],
            threat_entry_types: vec!["URL".to_string()],
            threat_entries: vec![ThreatEntry { url: url.to_string()}],
            },
    };


    let response: SafeBrowsingResponse = http_client
        .post(&format!("https://safebrowsing.googleapis.com/v4/threatMatches:find?key={}", api_key))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Safe browsing API request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse Safe Browsing response: {e}"))?;

    Ok(response.matches.is_none())
}

async fn get_api_key(secrets_client: &SecretsClient, secret_arn: &str) -> Result<String, String> {

    let secret_value = secrets_client
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await
        .map_err(|e| format!("Failed to retrieve API key: {e}"))?;

    // Return if there is something there in the secret
    secret_value
        .secret_string()
        .ok_or_else(|| "API key secret is empty".to_string())
        .map(|s| s.to_string())

}
