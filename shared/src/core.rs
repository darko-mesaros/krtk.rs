use std::collections::HashMap;

use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use aws_sdk_dynamodb::Client;
use cuid2::CuidConstructor;
use serde::{Deserialize, Serialize};
use chrono::Utc;

use crate::url_info::UrlInfo;

#[derive(Deserialize)]
pub struct ShortenUrlRequest {
    url_to_shorten: String,
}

impl ShortenUrlRequest {
    pub fn validate(self) -> Result<Self, String> {
        if !is_valid_url(&self.url_to_shorten) {
            return Err("Invalid URL Provided".to_string());
        }
        Ok(self)
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

// A struct that will contain info about our Short links
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

// We are implementing a TryFrom for ShortUrl
// TryFrom comes from the standard Rust library, its basically the same as From, but it can fail.
// Hence the Try. What we are doing here is Trying to convert HashMap<String, AttributeValue> to
// ShortUrl
impl TryFrom<HashMap<String, AttributeValue>> for ShortUrl {
    // Just a string for an error is fine for now
    type Error = String;

    // The TryFrom Trait requires us to implement a single method called try_from() - this.
    fn try_from(item: HashMap<String, AttributeValue>) -> Result<Self, Self::Error> {
        let link_id = item
            .get("LinkId")
            .ok_or_else(|| "LinkId not found".to_string())?
            .as_s()
            .map(|s| s.to_string())
            .map_err(|_| "LinkId is not a String".to_string())?;
        let original_link = item
            .get("OriginalLink")
            .ok_or_else(|| "OriginalLink not found".to_string())?
            .as_s()
            .map(|s| s.to_string())
            .map_err(|_| "OriginalLink is not a String".to_string())?;
        let clicks = item
            .get("Clicks")
            .ok_or_else(|| "Clicks not found".to_string())?
            .as_n() // Returns the Number (DynamoDB attr) as String
            .map_err(|_| "Clicks is not a Number".to_string())
            .and_then(|n| {
                // We then try to conver it into an actual u32
                n.parse::<u32>()
                    .map_err(|_| "Cannot convert Clicks into u32".to_string())
            })?;

        // Why are we not doing as much checking as above? Is it because these are Options?
        let content_type = item
            .get("ContentType")
            .and_then(|c| c.as_s().map(|s| s.to_string()).ok());
        let title = item
            .get("Title")
            .and_then(|c| c.as_s().map(|s| s.to_string()).ok());
        let description = item
            .get("Description")
            .and_then(|c| c.as_s().map(|s| s.to_string()).ok());
        let image = item
            .get("Image")
            .and_then(|c| c.as_s().map(|s| s.to_string()).ok());
        let timestamp = item
            .get("TimeStamp")
            .ok_or_else(|| "TimeStamp not found".to_string())?
            .as_n() // Returns the Number (DynamoDB attr) as String
            .map_err(|_| "TimeStamp is not a Number".to_string())
            .and_then(|n| {
                // We then try to conver it into an actual u32
                n.parse::<i64>()
                    .map_err(|_| "Cannot convert TimeStamp into i64".to_string())
            })?;
        Ok(Self {
            link_id,
            original_link,
            clicks,
            title,
            description,
            content_type,
            image,
            timestamp
        })
    }
}
// We are passing the DDB client as well as the table name in the UrlShortener struct.
// As this makes sense, this is the only thing in our app that will use the client.
#[derive(Debug)]
pub struct UrlShortener {
    dynamodb_urls_table: String,
    dynamodb_client: Client,
}

impl UrlShortener {
    pub fn new(dynamodb_urls_table: &str, dynamodb_client: Client) -> Self {
        Self {
            dynamodb_urls_table: dynamodb_urls_table.to_string(),
            dynamodb_client,
        }
    }

    // Only passing UrlInfo (and the HTTP client when I need to)
    pub async fn shorten_url(
        &self,
        req: ShortenUrlRequest,
        url_info: &UrlInfo,
    ) -> Result<ShortUrl, String> {

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
            .item("SortKey".to_string(), AttributeValue::S("LINKS".to_string())) // Adding the sort
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
            .map_err(|e| format!("Error adding item: {:?}", e)) // OR if there is an error, mapping
                                                                // it to this formatted string.
    }
    // Get the url from DynamoDB AND increment the count
    pub async fn retrieve_url_and_increment_count(
        &self,
        short_url: &str,
    ) -> Result<Option<String>, String> {
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
            .await
            .map(|record| {
                // Succesfull retrieve from DynamoDB
                record.attributes.and_then(|attributes| {
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
                // Generate a generic Error message just in case.
                let generic_err_msg = format!("Error incrementing clicks: {:?}", e);

                // Checking if the error is Conditional Check failed Exception, basically if the
                // URL does not exist. We do not want to error out just return none so we can 404
                if e.into_service_error()
                    .is_conditional_check_failed_exception()
                {
                    // Just return None
                    Ok(None)
                } else {
                    // Otherwise just return a generic error message
                    Err(generic_err_msg)
                }
            }
            Ok(result) => Ok(result),
        }
    }

    pub async fn list_urls(
        &self,
        last_evaluated_id: Option<&str>,
        last_evaluated_timestamp: Option<&str>,
    ) -> Result<ListShortUrlResponse, String> {
        // // Run a scan on 25 items, but make it mutable as we may do something in a bit.
        // let mut scan = self
        //     .dynamodb_client
        //     .scan()
        //     .table_name(&self.dynamodb_urls_table)
        //     .limit(25);
        //
        // // If we have a last_evaluated_id as Some() modify the scan to include the
        // // exclusive_start_key() with a value of the last_evaluated_id
        // if let Some(lei) = last_evaluated_id {
        //     scan = scan.exclusive_start_key("LinkId", AttributeValue::S(lei.to_string()));
        // }
        //
        // // Run the scan
        // let result = scan
        //     .send()
        //     .await
        //     .map_err(|e| format!("Error executing scan: {:?}", e))?;
        // Run a scan on 25 items, but make it mutable as we may do something in a bit.
        let mut query = self
            .dynamodb_client
            .query()
            .index_name("TimeStampIndex")
            .key_condition_expression("#pk = :pk")
            .expression_attribute_names("#pk", "SortKey")
            .expression_attribute_values(
                ":pk",
                AttributeValue::S("LINKS".to_string())
            )
            .table_name(&self.dynamodb_urls_table)
            .scan_index_forward(false)
            .limit(5);

        // If we have a last_evaluated_id as Some() modify the scan to include the
        // exclusive_start_key() with a value of the last_evaluated_id
        if let (Some(lei), Some(letime)) = (last_evaluated_id, last_evaluated_timestamp) {
            let mut exclusive_start_key = HashMap::new();
            exclusive_start_key.insert("SortKey".to_string(), AttributeValue::S("LINKS".to_string()));
            exclusive_start_key.insert("LinkId".to_string(), AttributeValue::S(lei.to_string()));
            exclusive_start_key.insert("TimeStamp".to_string(), AttributeValue::N(letime.to_string()));
            query = query.set_exclusive_start_key(Some(exclusive_start_key));
        }

        // Run the scan
        let result = query
            .send()
            .await
            .map_err(|e| format!("Error executing scan: {:?}", e))?;

        // An empty vector to store all teh short_urls
        let mut short_urls = vec![];

        // If we get somethign back lets do the try_from() for them into the ShortUrl struct
        if let Some(items) = result.items {
            for item in items {
                // ingore the items that cannot be deserialized - for the ones that are broken,
                // (bad data, missing fields), just ignore them.
                if let Ok(short_url) = ShortUrl::try_from(item) {
                    // Add to the vector of ShortUrl
                    short_urls.push(short_url);
                }
            }
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
        let idgen = CuidConstructor::new().with_length(10);
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
            parsed.scheme() == "http" || parsed.scheme() == "https"
        } else {
            false
        }
    }
