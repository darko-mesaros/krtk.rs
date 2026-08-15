use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;
use clap::Parser;
use shared::core::owner_key;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "migrate_owners", about = "Assign OwnerId to existing links")]
struct Args {
    /// DynamoDB table name
    #[arg(long)]
    table: String,

    /// Cognito sub to assign as owner
    #[arg(long)]
    owner_sub: String,

    /// Scan and report what would be changed, but write nothing
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// AWS region (defaults to us-west-2)
    #[arg(long, default_value = "us-west-2")]
    region: String,
}

struct MigrationStats {
    inspected: u64,
    updated: u64,
    skipped: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(args.region.clone()))
        .load()
        .await;
    let client = Client::new(&config);

    let stats = run_migration(&client, &args).await;

    if args.dry_run {
        println!("DRY RUN -- no changes written.");
        println!("  Inspected:    {}", stats.inspected);
        println!("  Would update: {}", stats.updated);
        println!("  Already owned: {}", stats.skipped);
    } else {
        println!("Migration complete.");
        println!("  Inspected: {}", stats.inspected);
        println!("  Updated:   {}", stats.updated);
        println!("  Skipped:   {} (already had OwnerId)", stats.skipped);
    }
}

async fn run_migration(client: &Client, args: &Args) -> MigrationStats {
    let mut stats = MigrationStats {
        inspected: 0,
        updated: 0,
        skipped: 0,
    };

    let mut exclusive_start_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

    loop {
        let mut scan = client
            .scan()
            .table_name(&args.table)
            .projection_expression("LinkId, OwnerId");

        if let Some(ref start_key) = exclusive_start_key {
            scan = scan.set_exclusive_start_key(Some(start_key.clone()));
        }

        let response = scan.send().await.unwrap_or_else(|e| {
            panic!("Scan failed: {e}");
        });

        for item in response.items() {
            stats.inspected += 1;

            if needs_migration(item) {
                if args.dry_run {
                    stats.updated += 1;
                } else {
                    let link_id = item
                        .get("LinkId")
                        .expect("LinkId must be present in scan result");

                    match update_owner(client, &args.table, link_id, &args.owner_sub).await {
                        UpdateResult::Updated => stats.updated += 1,
                        UpdateResult::AlreadyOwned => {
                            info!(
                                link_id = ?link_id,
                                "Condition check failed (race): already owned"
                            );
                            stats.skipped += 1;
                        }
                        UpdateResult::Failed(e) => {
                            warn!(link_id = ?link_id, error = %e, "Update failed");
                            panic!("Storage operation failed: {e}");
                        }
                    }
                }
            } else {
                stats.skipped += 1;
            }
        }

        exclusive_start_key = response.last_evaluated_key().map(|k| k.to_owned());
        if exclusive_start_key.is_none() {
            break;
        }
    }

    stats
}

/// Determines whether an item needs migration (OwnerId is absent or null).
fn needs_migration(item: &std::collections::HashMap<String, AttributeValue>) -> bool {
    match item.get("OwnerId") {
        None => true,
        Some(val) => val.as_null().is_ok(),
    }
}

enum UpdateResult {
    Updated,
    AlreadyOwned,
    Failed(String),
}

async fn update_owner(
    client: &Client,
    table: &str,
    link_id: &AttributeValue,
    owner_sub: &str,
) -> UpdateResult {
    let sort_key_value = owner_key(owner_sub);

    let result = client
        .update_item()
        .table_name(table)
        .key("LinkId", link_id.clone())
        .update_expression("SET OwnerId = :sub, SortKey = :ownerKey")
        .condition_expression("attribute_not_exists(OwnerId)")
        .expression_attribute_values(":sub", AttributeValue::S(owner_sub.to_string()))
        .expression_attribute_values(":ownerKey", AttributeValue::S(sort_key_value))
        .send()
        .await;

    match result {
        Ok(_) => UpdateResult::Updated,
        Err(e) => {
            let service_err = e.into_service_error();
            if service_err.is_conditional_check_failed_exception() {
                UpdateResult::AlreadyOwned
            } else {
                UpdateResult::Failed("Storage operation failed".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::types::AttributeValue;
    use std::collections::HashMap;

    #[test]
    fn item_without_owner_id_needs_migration() {
        let item: HashMap<String, AttributeValue> = HashMap::from([(
            "LinkId".to_string(),
            AttributeValue::S("abc123".to_string()),
        )]);
        assert!(needs_migration(&item));
    }

    #[test]
    fn item_with_null_owner_id_needs_migration() {
        let item: HashMap<String, AttributeValue> = HashMap::from([
            ("LinkId".to_string(), AttributeValue::S("abc123".to_string())),
            ("OwnerId".to_string(), AttributeValue::Null(true)),
        ]);
        assert!(needs_migration(&item));
    }

    #[test]
    fn item_with_owner_id_does_not_need_migration() {
        let item: HashMap<String, AttributeValue> = HashMap::from([
            ("LinkId".to_string(), AttributeValue::S("abc123".to_string())),
            (
                "OwnerId".to_string(),
                AttributeValue::S("some-sub".to_string()),
            ),
        ]);
        assert!(!needs_migration(&item));
    }

    #[test]
    fn owner_key_produces_expected_format() {
        let result = owner_key("abc-123-def");
        assert_eq!(result, "USER#abc-123-def");
    }
}
