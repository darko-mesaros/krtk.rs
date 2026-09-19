use std::env;
use std::io::Read;

use aws_lambda_events::event::s3::S3Event;
use flate2::read::GzDecoder;
use lambda_runtime::{run, service_fn, tracing, Error, LambdaEvent};
use shared::core::UrlShortener;

mod parser;

use parser::{parse_log_object, plan_increments};

/// Handles one S3 `ObjectCreated` batch of CloudFront access-log objects.
///
/// Per record: claim the object first (idempotency, FR-5), and only if the claim is
/// won do we fetch, gunzip, parse, aggregate, and apply one `increment_click_count_by`
/// per distinct link (FR-7). Nothing here touches the redirect path, so a total
/// failure of this function leaves redirects working and only stales click counts
/// (FR-3).
pub async fn function_handler(
    url_shortener: &UrlShortener,
    s3_client: &aws_sdk_s3::Client,
    event: LambdaEvent<S3Event>,
) -> Result<(), Error> {
    for record in event.payload.records {
        let bucket = match record.s3.bucket.name {
            Some(name) => name,
            None => {
                tracing::warn!("S3 event record had no bucket name; skipping");
                continue;
            }
        };
        let key = match record.s3.object.key {
            Some(key) => key,
            None => {
                tracing::warn!("S3 event record had no object key; skipping");
                continue;
            }
        };

        // Claim BEFORE processing. If already claimed, this is a redelivery of an
        // object we have handled: log at info and skip the whole object (FR-5.1).
        let claimed = match url_shortener.claim_log_object(&key).await {
            Ok(claimed) => claimed,
            Err(e) => {
                // Could not even claim it. Warn (feeds the alarm) and move on; the
                // redirect path is unaffected.
                tracing::warn!("Failed to claim log object {}: {:?}", key, e);
                continue;
            }
        };
        if !claimed {
            tracing::info!("Log object {} already claimed; skipping", key);
            continue;
        }

        let decompressed = match fetch_and_decompress(s3_client, &bucket, &key).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("Failed to read log object {}: {:?}", key, e);
                continue;
            }
        };

        let link_ids = parse_log_object(&decompressed);
        // Gate the aggregate on the claim result through the tested pure seam.
        let counts = plan_increments(claimed, &link_ids);

        // One UpdateItem per distinct link. A per-link failure warns (feeds the alarm,
        // FR-9) and does not abort the batch.
        for (link_id, count) in counts {
            if let Err(e) = url_shortener.increment_click_count_by(&link_id, count).await {
                tracing::warn!(
                    "Failed to increment click count for {} by {}: {:?}",
                    link_id,
                    count,
                    e
                );
            }
        }
    }

    Ok(())
}

/// Fetches an S3 object and gunzips it to a UTF-8 string.
///
/// CloudFront standard access-log objects are gzipped, so the body is inflated with
/// `GzDecoder` before parsing. Kept separate from the handler so the AWS-touching
/// step is small and the parsing core stays pure and testable.
async fn fetch_and_decompress(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> Result<String, Error> {
    let object = s3_client.get_object().bucket(bucket).key(key).send().await?;
    let body = object.body.collect().await?.into_bytes();

    let mut decoder = GzDecoder::new(&body[..]);
    let mut decompressed = String::new();
    decoder.read_to_string(&mut decompressed)?;

    Ok(decompressed)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();

    let table_name = env::var("TABLE_NAME").expect("No TABLE_NAME environment variable set");
    let shortener_domain =
        env::var("SHORTENER_DOMAIN").expect("No SHORTENER_DOMAIN environment variable set");

    let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
        .load()
        .await;
    let dynamodb_client = aws_sdk_dynamodb::Client::new(&config);
    let s3_client = aws_sdk_s3::Client::new(&config);

    let shortener = UrlShortener::new(&table_name, &shortener_domain, dynamodb_client);

    run(service_fn(|event| {
        function_handler(&shortener, &s3_client, event)
    }))
    .await
}
