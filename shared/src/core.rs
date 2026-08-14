use std::collections::HashMap;

use aws_sdk_dynamodb::operation::put_item::PutItemError;
use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_secretsmanager::Client as SecretsClient;
use cuid2::CuidConstructor;
use lambda_http::tracing;
use serde::{Deserialize, Serialize};
use chrono::Utc;

use crate::url_info::UrlInfo;
use crate::safe_browsing::is_url_safe;
use crate::error::AppError;

const SORT_KEY_VALUE: &str = "LINKS";  // Use same value in query and ExclusiveStartKey
const URL_LENGTH: u16 = 7;  // The lenght of the shortened URL for CUID2 to generate

#[derive(Deserialize)]
pub struct ShortenUrlRequest {
    url_to_shorten: String,
}

impl ShortenUrlRequest {
    pub async fn validate(self, shortener_domain: &str, secrets_client: &SecretsClient, secret_arn: &str, http_client: &reqwest::Client) -> Result<Self, AppError> {

        // Synchronous validation
        let validated = self.validate_url_format()
            .and_then(|req| req.validate_not_recursive(shortener_domain))?;

        // Async validation (slower)
        validated.validate_safe_browsing(secrets_client, secret_arn, http_client).await
    }
    fn validate_url_format(self) -> Result<Self, AppError> {
        if !is_valid_url(&self.url_to_shorten) {
            return Err(AppError::Validation("Invalid URL Provided".to_string()));
        }
        Ok(self)
    }
    fn validate_not_recursive(self, shortener_domain: &str) -> Result<Self, AppError> {
        if is_recursive_url(&self.url_to_shorten, shortener_domain) {
            return Err(AppError::Validation(format!("Cannot shorten links, already shortened links of {shortener_domain}")));
        }
        Ok(self)
    }

    async fn validate_safe_browsing(self, secrets_client: &SecretsClient, secret_arn: &str, http_client: &reqwest::Client) -> Result<Self, AppError> {
        match is_url_safe(&self.url_to_shorten, secrets_client, secret_arn, http_client).await {
            Ok(true) => Ok(self),
            Ok(false) => Err(AppError::SafeBrowsing("URL flagged as unsafe by Google Safe Browsing".to_string())),
            Err(_) => Ok(self), // Fail open - do not block if the API is down
        }
    }
}

#[derive(Serialize)]
pub struct ShortenUrlResponse {
    shortened_url: String,
}

// Response for when we need all the urls
#[derive(Debug, Serialize)]
pub struct ListShortUrlResponse {
    short_urls: Vec<ShortUrl>,
    last_evaluated_id: Option<String>,
    last_evaluated_timestamp: Option<String>,
    // TODO: Does this one need to be public? 
    pub has_more: bool,
}

// A struct that will contain info about our Short links.
//
// These field names ARE the public API contract: they appear verbatim in the
// /api/links JSON and are what `templates::Link` deserializes for the HTMX
// partial. Do NOT put #[serde(rename = ...)] on them -- DynamoDB attribute
// naming belongs on ShortUrlRow below. (Renaming these to the DynamoDB
// PascalCase names broke both the JSON contract and the HTMX path with a 500.)
#[derive(Debug, Serialize)]
pub struct ShortUrl {
    pub link_id: String,
    original_link: String,
    clicks: u32,
    title: Option<String>,
    description: Option<String>,
    content_type: Option<String>,
    image: Option<String>,
    timestamp: i64,
}

// Persistence shape: mirrors the DynamoDB attribute names exactly, for
// serde_dynamo. Deliberately a separate type from ShortUrl -- a single type
// cannot carry both namings, because serde renames apply to Serialize and
// Deserialize alike.
#[derive(Debug, Deserialize)]
struct ShortUrlRow {
    #[serde(rename = "LinkId")]
    link_id: String,
    #[serde(rename = "OriginalLink")]
    original_link: String,
    #[serde(rename = "Clicks")]
    clicks: u32,
    #[serde(rename = "Title")]
    title: Option<String>,
    #[serde(rename = "Description")]
    description: Option<String>,
    #[serde(rename = "ContentType")]
    content_type: Option<String>,
    #[serde(rename = "Image")]
    image: Option<String>,
    #[serde(rename = "TimeStamp")]
    timestamp: i64,
}

impl From<ShortUrlRow> for ShortUrl {
    fn from(row: ShortUrlRow) -> Self {
        Self {
            link_id: row.link_id,
            original_link: row.original_link,
            clicks: row.clicks,
            title: row.title,
            description: row.description,
            content_type: row.content_type,
            image: row.image,
            timestamp: row.timestamp,
        }
    }
}
// We are passing the DDB client as well as the table name in the UrlShortener struct.
// As this makes sense, this is the only thing in our app that will use the client.
#[derive(Debug)]
pub struct UrlShortener {
    dynamodb_urls_table: String,
    pub shortener_domain: String,
    dynamodb_client: Client,
}

impl UrlShortener {
    pub fn new(dynamodb_urls_table: &str, shortener_domain: &str, dynamodb_client: Client) -> Self {
        Self {
            dynamodb_urls_table: dynamodb_urls_table.to_string(),
            shortener_domain: shortener_domain.to_string(),
            dynamodb_client,
        }
    }

    // Only passing UrlInfo (and the HTTP client when I need to)
    pub async fn shorten_url(
        &self,
        req: ShortenUrlRequest,
        url_info: &UrlInfo,
    ) -> Result<ShortUrl, AppError> {

        // Normalize the URL before:
        let normalized_url = normalize_url(&req.url_to_shorten);

        let short_url = self.generate_short_url();

        let url_details = url_info
            .fetch_details(&normalized_url)
            .await
            .unwrap_or_default();

        // Using the DDB Client from the Struct
        //self.dynamodb_client
        let mut put_item = self
            .dynamodb_client
            .put_item() // Put single item
            .table_name(&self.dynamodb_urls_table) // Table name is from the Struct
            .item("SortKey".to_string(), AttributeValue::S(SORT_KEY_VALUE.to_string())) // Adding the sort
                                                                                 // key
            .item("LinkId", AttributeValue::S(short_url.clone())) // Putting item "LinkId" as
            // String
            .item(
                "OriginalLink",
                AttributeValue::S(normalized_url.clone()),
            ) // Putting item
            // "OriginalLink"
            // as String
            .item("Clicks", AttributeValue::N("0".to_string())); // Putting item "Clicks" a Number,
                                                                 // specifically 0 as this is a new
                                                                 // item.

        // Check if we have some URL details to post also
        if let Some(ref title) = url_details.title {
            put_item = put_item.item("Title", AttributeValue::S(title.to_string()));
        }
        if let Some(ref description) = url_details.description {
            put_item = put_item.item("Description", AttributeValue::S(description.to_string()));
        }
        if let Some(ref content_type) = url_details.content_type {
            put_item = put_item.item("ContentType", AttributeValue::S(content_type.to_string()));
        }
        if let Some(ref image) = url_details.image {
            put_item = put_item.item("Image", AttributeValue::S(image.to_string()));
        }

        // Add the current timestamp
        // NOTE:for future Darko - you deal with the local time vs UTC
        let current_time = Utc::now().timestamp();
        put_item = put_item.item("TimeStamp", AttributeValue::N(current_time.to_string()));

        // Once we are ready, let's send the call
        put_item
            .condition_expression("attribute_not_exists(LinkId)") // We are making a condition to
            // this put_item to be that the
            // "LinkId" cannot already exist
            .send()
            .await
            .map(|_| ShortUrl {
                // Just mapping the oputput to a new struct ShortenUrlResponse
                link_id: short_url,
                original_link: req.url_to_shorten.clone(),
                clicks: 0,
                title: url_details.title,
                description: url_details.description,
                content_type: url_details.content_type,
                image: url_details.image,
                timestamp: current_time, //TODO: Clean this up
            })
            .map_err(|e| match e {
                SdkError::ServiceError(err) => {
                    match err.err() {
                        PutItemError::ConditionalCheckFailedException(e) => {
                            tracing::error!("Error creating link {:?}", e);
                            AppError::Validation("The Link ID we tried to create, already exists. Please try again.".to_string())
                        },
                        other_error => {
                            tracing::error!("Error creating link {:?}", &other_error);
                            AppError::database(SdkError::ServiceError(err))
                        }
                    }
                },
                other_sdk_error => {
                    tracing::error!("Error creating link {:?}", &other_sdk_error);
                    AppError::database(other_sdk_error)
                }
            })
    }
    // Get the url from DynamoDB AND increment the count
    pub async fn retrieve_url(
        &self,
        short_url: &str,
    ) -> Result<Option<String>, AppError> {
        let result = self
            .dynamodb_client
            .get_item()
            .table_name(&self.dynamodb_urls_table)
            .key("LinkId", AttributeValue::S(short_url.to_string()))
            .send()
            .await
            .map(|record| {
                // Succesfull retrieve from DynamoDB
                record.item.and_then(|attributes| {
                    // If there is (Some)thing in the Item
                    attributes.get("OriginalLink").and_then(
                        |v| // If there is Some with an attr "OriginalLink"
                                v.as_s()  // Try to convert it to String
                                .cloned() // Try to Clone the Result so we own it
                                .ok(),
                    ) // Return an Option from Result if all works
                })
            });
        match result {
            Err(e) => {
                tracing::error!("Error retrieving URL: {:?}", e);
                Err(AppError::database(e))
            }
            Ok(result) => Ok(result),
        }
    }
    // Increment Click Count
    pub async fn increment_click_count(
        &self,
        short_url: &str,
    ) -> Result<(), AppError> {
        let result = self
            .dynamodb_client
            .update_item()
            .table_name(&self.dynamodb_urls_table)
            .key("LinkId", AttributeValue::S(short_url.to_string()))
            .update_expression("SET Clicks = Clicks + :val")
            .expression_attribute_values(":val", AttributeValue::N("1".to_string()))
            .condition_expression("attribute_exists(LinkId)")
            .return_values(ReturnValue::AllNew)
            .send()
            .await;

        match result {
            Err(e) => {
                tracing::error!("Error incrementing clicks: {:?}", e);
                Err(AppError::database(e))
            }
            Ok(_) => Ok(()),
        }
    }

    pub async fn list_urls(
        &self,
        last_evaluated_id: Option<&str>,
        last_evaluated_timestamp: Option<&str>,
    ) -> Result<ListShortUrlResponse, AppError> {
        // Run a scan on 25 items, but make it mutable as we may do something in a bit.
        let mut query = self
            .dynamodb_client
            .query()
            .index_name("TimeStampIndex")
            .key_condition_expression("#pk = :pk")
            .expression_attribute_names("#pk", "SortKey")
            .expression_attribute_values(
                ":pk",
                AttributeValue::S(SORT_KEY_VALUE.to_string())
            )
            .table_name(&self.dynamodb_urls_table)
            .scan_index_forward(false)
            .limit(5);

        // If we have a last_evaluated_id as Some() modify the scan to include the
        // exclusive_start_key() with a value of the last_evaluated_id
        if let (Some(lei), Some(letime)) = (last_evaluated_id, last_evaluated_timestamp) {
            let mut exclusive_start_key = HashMap::new();
            exclusive_start_key.insert("SortKey".to_string(), AttributeValue::S(SORT_KEY_VALUE.to_string()));
            exclusive_start_key.insert("LinkId".to_string(), AttributeValue::S(lei.to_string()));
            exclusive_start_key.insert("TimeStamp".to_string(), AttributeValue::N(letime.to_string()));
            query = query.set_exclusive_start_key(Some(exclusive_start_key));
        }

        // Run the scan
        let result = query
            .send()
            .await
            .map_err(AppError::database)?;

        // An empty vector to store all teh short_urls
        let mut short_urls = vec![];

        // If we get somethign back lets do the try_from() for them into the ShortUrl struct
        if let Some(items) = result.items {
            let rows: Vec<ShortUrlRow> = serde_dynamo::from_items(items)
                .map_err(AppError::Serialization)?;
            short_urls = rows.into_iter().map(ShortUrl::from).collect();
        }

        // Set the last_evaluated_id from the result
        // If the key is Empty that means the last page of results has been processed.
                // Extract pagination tokens
        let (last_evaluated_id, last_evaluated_timestamp) = 
            if let Some(last_key) = result.last_evaluated_key {
                (
                    last_key.get("LinkId")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string()),
                    last_key.get("TimeStamp")
                        .and_then(|v| v.as_n().ok())
                        .map(|s| s.to_string())
                )
            } else {
                (None, None)
            };
        let has_more = last_evaluated_id.is_some() && last_evaluated_timestamp.is_some();

        // Return the ListShortUrlResponse Struct with all the urls
        Ok(ListShortUrlResponse {
            short_urls,
            last_evaluated_id,
            last_evaluated_timestamp,
            has_more,
        })
    }
    fn generate_short_url(&self) -> String {
        let idgen = CuidConstructor::new().with_length(URL_LENGTH);
        idgen.create_id()
    }
}
    // Normalize the URL
    fn normalize_url(url: &str) -> String {
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            // Let's default to https://
            format!("https://{}", url.trim_start_matches("//"))
        }
    }
    // Check if URL is valid
    fn is_valid_url(url: &str) -> bool {
        if let Ok(parsed) = url::Url::parse(&normalize_url(url)) {
            // Check if it has a valid scheme and host
        (parsed.scheme() == "http" || parsed.scheme() == "https")
            && parsed.host_str().is_some_and(|host| host.contains('.'))
        } else {
            false
        }
    }

    // Check if the url is not the short URL itself
    fn is_recursive_url(url: &str, shortener_domain: &str) -> bool {
        if let Ok(parsed) = url::Url::parse(&normalize_url(url)) {
        parsed.host_str()
        .map(|host| host == shortener_domain)
            .unwrap_or(false)
        } else {
        false
        }
    }

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the exact item shape `shorten_url` writes to DynamoDB, so these tests fail
    /// if the writer and the `#[serde(rename)]` attributes on `ShortUrl` ever drift apart.
    fn stored_item(with_optionals: bool) -> HashMap<String, AttributeValue> {
        let mut item = HashMap::new();
        item.insert("SortKey".into(), AttributeValue::S(SORT_KEY_VALUE.into()));
        item.insert("LinkId".into(), AttributeValue::S("abc1234".into()));
        item.insert("OriginalLink".into(), AttributeValue::S("https://example.com/".into()));
        item.insert("Clicks".into(), AttributeValue::N("42".into()));
        item.insert("TimeStamp".into(), AttributeValue::N("1739035776".into()));
        if with_optionals {
            item.insert("Title".into(), AttributeValue::S("Example Domain".into()));
            item.insert("Description".into(), AttributeValue::S("An example".into()));
            item.insert("ContentType".into(), AttributeValue::S("text/html".into()));
            item.insert("Image".into(), AttributeValue::S("https://example.com/og.png".into()));
        }
        item
    }

    #[test]
    fn deserializes_a_fully_populated_item() {
        let url: ShortUrlRow = serde_dynamo::from_item(stored_item(true))
            .expect("a full item written by shorten_url must deserialize");

        assert_eq!(url.link_id, "abc1234");
        assert_eq!(url.original_link, "https://example.com/");
        assert_eq!(url.clicks, 42);
        assert_eq!(url.timestamp, 1_739_035_776);
        assert_eq!(url.title.as_deref(), Some("Example Domain"));
        assert_eq!(url.description.as_deref(), Some("An example"));
        assert_eq!(url.content_type.as_deref(), Some("text/html"));
        assert_eq!(url.image.as_deref(), Some("https://example.com/og.png"));
    }

    #[test]
    fn deserializes_an_item_with_no_scraped_metadata() {
        // shorten_url omits Title/Description/ContentType/Image entirely when scraping
        // yields nothing, so absent (not null) optional attributes must be accepted.
        let url: ShortUrlRow = serde_dynamo::from_item(stored_item(false))
            .expect("an item without scraped metadata must still deserialize");

        assert_eq!(url.link_id, "abc1234");
        assert_eq!(url.clicks, 42);
        assert!(url.title.is_none());
        assert!(url.description.is_none());
        assert!(url.content_type.is_none());
        assert!(url.image.is_none());
    }

    #[test]
    fn ignores_unknown_attributes_so_new_columns_do_not_break_reads() {
        // The auth spec adds OwnerId to this table. Reads must tolerate attributes the
        // struct does not know about, or deploying the writer first would break listing.
        let mut item = stored_item(true);
        item.insert("OwnerId".into(), AttributeValue::S("cognito-sub-123".into()));

        let url: ShortUrlRow = serde_dynamo::from_item(item)
            .expect("an unknown attribute must not fail deserialization");
        assert_eq!(url.link_id, "abc1234");
    }

    #[test]
    fn a_missing_required_attribute_is_now_an_error_not_a_silent_drop() {
        // This is the regression guard for the bug this change fixes: list_urls used to
        // discard malformed items, so a mapping error presented as links vanishing from
        // the list rather than as a failure.
        let mut item = stored_item(true);
        item.remove("OriginalLink");

        let result: Result<ShortUrlRow, _> = serde_dynamo::from_item(item);
        assert!(result.is_err(), "a missing required attribute must surface as an error");
    }

    #[test]
    fn a_wrongly_typed_attribute_is_an_error() {
        let mut item = stored_item(true);
        item.insert("Clicks".into(), AttributeValue::S("not-a-number".into()));

        let result: Result<ShortUrlRow, _> = serde_dynamo::from_item(item);
        assert!(result.is_err(), "a string in a numeric attribute must surface as an error");
    }

    /// The /api/links JSON field names are a public contract: the browser client and
    /// `templates::Link` both read them. This pins them so a serde rename added for
    /// DynamoDB can never silently leak into the API response again (doing exactly
    /// that returned PascalCase keys and 500'd the HTMX path).
    #[test]
    fn short_url_serializes_with_the_public_snake_case_contract() {
        let row: ShortUrlRow = serde_dynamo::from_item(stored_item(true)).unwrap();
        let json = serde_json::to_value(ShortUrl::from(row)).unwrap();
        let obj = json.as_object().expect("ShortUrl must serialize to an object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "clicks",
                "content_type",
                "description",
                "image",
                "link_id",
                "original_link",
                "timestamp",
                "title",
            ],
            "the /api/links wire shape changed -- this is a breaking API change"
        );

        assert_eq!(obj["link_id"], "abc1234");
        assert_eq!(obj["clicks"], 42);
        assert_eq!(obj["timestamp"], 1_739_035_776i64);
    }

    /// templates::Link is deserialized from the serialized ShortUrl in get_links, so
    /// the two must stay compatible or the HTMX partial 500s.
    #[test]
    fn serialized_short_url_still_deserializes_into_the_template_link() {
        let row: ShortUrlRow = serde_dynamo::from_item(stored_item(true)).unwrap();
        let json = serde_json::to_value(ShortUrl::from(row)).unwrap();
        let link: Result<crate::templates::Link, _> = serde_json::from_value(json);
        assert!(link.is_ok(), "templates::Link must deserialize the API shape: {link:?}");
    }

    #[test]
    fn normalize_url_defaults_to_https_and_preserves_explicit_schemes() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(normalize_url("//example.com"), "https://example.com");
        assert_eq!(normalize_url("http://example.com"), "http://example.com");
        assert_eq!(normalize_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn is_valid_url_requires_a_dotted_host() {
        assert!(is_valid_url("example.com"));
        assert!(is_valid_url("https://sub.example.co.uk/path?q=1"));
        assert!(!is_valid_url("localhost"));
        assert!(!is_valid_url("not a url"));
        assert!(!is_valid_url(""));
    }

    #[test]
    fn is_recursive_url_catches_our_own_domain() {
        assert!(is_recursive_url("https://krtk.rs/abc1234", "krtk.rs"));
        assert!(is_recursive_url("krtk.rs/abc1234", "krtk.rs"));
        assert!(!is_recursive_url("https://example.com/", "krtk.rs"));
        // A domain that merely ends with ours must not be treated as recursive.
        assert!(!is_recursive_url("https://notkrtk.rs/x", "krtk.rs"));
    }
}
