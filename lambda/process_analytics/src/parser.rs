//! Pure, AWS-free parsing of CloudFront standard (v2, JSON) access-log objects.
//!
//! Every function here is a pure transformation over strings, so the whole
//! classification and aggregation core is unit-testable with no S3, no DynamoDB,
//! and no Lambda runtime (FR-6.3). The handler in `main.rs` is the only part that
//! touches AWS; it fetches and gunzips the object, then hands the decompressed
//! text to `parse_log_object`.
//!
//! Fields are addressed BY NAME via serde renames, never by column index. This is
//! the whole point of choosing JSON delivery over the fixed-column TSV: a CloudFront
//! field-order change can no longer silently corrupt counts or panic the way the old
//! `fields[3]` positional parser did (FR-6.1).

use std::collections::HashMap;

use serde::Deserialize;

/// Reserved exact paths that are site chrome, never a short-link id.
const RESERVED_EXACT: [&str; 4] = ["/index.html", "/terms", "/privacy", "/"];

/// Reserved path prefixes (API, static assets, auth callback) that are never clicks.
const RESERVED_PREFIXES: [&str; 3] = ["/api/", "/assets/", "/auth/"];

/// One CloudFront standard access-log record, as one JSON object per line.
///
/// The three required fields drive classification. `timestamp(ms)` and `c-country`
/// are selected in the delivery config and carried here for the deferred per-link
/// analytics (FR-11.2); they are deserialized but not consumed by this change.
/// They are `Option` so that a record missing them (which should not happen given
/// the fixed `recordFields`) is still usable for click counting rather than dropped.
#[derive(Debug, Deserialize)]
pub struct LogRecord {
    #[serde(rename = "sc-status")]
    pub sc_status: String,
    #[serde(rename = "cs-uri-stem")]
    pub cs_uri_stem: String,
    #[serde(rename = "cs-method")]
    pub cs_method: String,
    #[serde(rename = "timestamp(ms)")]
    #[allow(dead_code)] // Carried for deferred analytics (FR-11.2), unused this change.
    pub timestamp_ms: Option<String>,
    #[serde(rename = "c-country")]
    #[allow(dead_code)] // Carried for deferred analytics (FR-11.2), unused this change.
    pub c_country: Option<String>,
}

/// The exact length of a generated short-link id: cuid2 built with
/// `CuidConstructor::new().with_length(7)` in `shared::core` (URL_LENGTH = 7).
const SHORT_LINK_LEN: usize = 7;

/// Returns `Some(link_id)` if and only if this record is a countable short-link
/// click, else `None`.
///
/// A record counts when ALL hold (FR-4.2):
///   - method is GET (a HEAD or OPTIONS to a short link is not a click),
///   - status is 302 (the redirect the visit_link Lambda issues),
///   - the path is exactly a leading '/' followed by 7 chars matching `^[a-z0-9]{7}$`
///     (the shape a cuid2 id can have, see `is_short_link_path`),
///   - the path is not one of the reserved exact paths, and
///   - the path does not start with a reserved prefix.
///
/// Because standard logging covers the ENTIRE distribution (not just the `/?*`
/// behaviour the old realtime config was scoped to), this filter is load-bearing
/// (FR-4.4). The whole distribution is scanned by internet bots probing for files
/// like `/compose.yaml`, `/stripe-keys.json` or `/phpinfo`; those get a 302 to the
/// SPA and, under a loose single-segment rule, were classified as clicks and then
/// failed the `attribute_exists(LinkId)` increment. Requiring the exact cuid2 shape
/// drops every such probe that carries a dot, hyphen, underscore or the wrong length
/// before it ever reaches DynamoDB. The link id is `cs-uri-stem` with the leading
/// '/' stripped; any query string lives in a separate `cs-uri-query` field, so no
/// '?' splitting is needed.
pub fn classify(record: &LogRecord) -> Option<String> {
    if record.cs_method != "GET" {
        return None;
    }
    if record.sc_status != "302" {
        return None;
    }

    let path = record.cs_uri_stem.as_str();

    // Must have the exact cuid2 short-link shape: '/' + 7 chars of [a-z0-9].
    if !is_short_link_path(path) {
        return None;
    }
    // These can never match a 7-char [a-z0-9] segment, but the exclusions stay as a
    // deliberate second gate so tightening the shape did not silently drop them.
    if RESERVED_EXACT.contains(&path) {
        return None;
    }
    if RESERVED_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return None;
    }

    Some(path.trim_start_matches('/').to_string())
}

/// `^/[a-z0-9]{7}$`: a leading '/', then exactly 7 characters each in [a-z0-9].
///
/// This is the shape a cuid2 id generated with length 7 can take (lowercase letters
/// and digits only). It is intentionally a shape check, not a table lookup: a path
/// that happens to be 7 lowercase alphanumerics but is not a real id (say `phpinfo`)
/// still fails the downstream `attribute_exists(LinkId)` condition quietly, and
/// chasing those with a bot blocklist would be over-engineering. The job here is only
/// to exclude the obvious non-ids (dots, hyphens, underscores, wrong length).
fn is_short_link_path(path: &str) -> bool {
    match path.strip_prefix('/') {
        Some(rest) => {
            rest.len() == SHORT_LINK_LEN
                && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        }
        None => false,
    }
}

/// Parses a decompressed log object into the list of countable link ids.
///
/// Splits into lines, deserializes each as a `LogRecord`, and SKIPS any line that
/// fails to deserialize (the `#Fields:` / `#Version:` headers CloudFront may emit,
/// blank lines, truncated tail lines) without panicking (FR-6.2). Surviving records
/// are run through `classify`; the `Some` link ids are collected in order.
pub fn parse_log_object(decompressed: &str) -> Vec<String> {
    decompressed
        .lines()
        .filter_map(|line| serde_json::from_str::<LogRecord>(line).ok())
        .filter_map(|record| classify(&record))
        .collect()
}

/// Aggregates a flat list of link-id hits into a per-link count.
///
/// This is what makes one log object cost at most one `UpdateItem` per distinct link
/// rather than one per hit (FR-7): 50 hits on link `a` become `{a: 50}`.
pub fn aggregate(link_ids: &[String]) -> HashMap<String, u64> {
    let mut counts = HashMap::new();
    for id in link_ids {
        *counts.entry(id.clone()).or_insert(0) += 1;
    }
    counts
}

/// The increments this invocation should apply, given whether it won the object claim.
///
/// This is the pure seam for the idempotency guarantee (FR-12.4): if `claimed` is
/// false the object was already processed by a prior delivery, so the plan is empty
/// and NO increments happen. If true, the plan is the per-link aggregate. Splitting
/// this out lets the "already claimed -> zero increments" branch be proven without a
/// live DynamoDB table -- the handler wires `claim_log_object`'s result into it.
pub fn plan_increments(claimed: bool, link_ids: &[String]) -> HashMap<String, u64> {
    if claimed {
        aggregate(link_ids)
    } else {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `LogRecord` from the three fields that matter for classification;
    /// the carried analytics fields are populated so tests exercise the real shape.
    fn record(method: &str, status: &str, uri: &str) -> LogRecord {
        LogRecord {
            sc_status: status.to_string(),
            cs_uri_stem: uri.to_string(),
            cs_method: method.to_string(),
            timestamp_ms: Some("1739035776180".to_string()),
            c_country: Some("US".to_string()),
        }
    }

    #[test]
    fn valid_short_link_302_get_is_counted() {
        assert_eq!(
            classify(&record("GET", "302", "/k3m9x2p")),
            Some("k3m9x2p".to_string())
        );
    }

    #[test]
    fn api_path_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/api/links")), None);
    }

    #[test]
    fn assets_path_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/assets/main.js")), None);
    }

    #[test]
    fn auth_path_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/auth/callback")), None);
    }

    #[test]
    fn index_html_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/index.html")), None);
    }

    #[test]
    fn terms_and_privacy_are_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/terms")), None);
        assert_eq!(classify(&record("GET", "302", "/privacy")), None);
    }

    #[test]
    fn bare_root_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/")), None);
    }

    #[test]
    fn a_404_on_a_short_path_is_not_counted() {
        assert_eq!(classify(&record("GET", "404", "/k3m9x2p")), None);
    }

    #[test]
    fn a_200_on_a_short_path_is_not_counted() {
        assert_eq!(classify(&record("GET", "200", "/k3m9x2p")), None);
    }

    #[test]
    fn a_head_302_is_not_counted() {
        assert_eq!(classify(&record("HEAD", "302", "/k3m9x2p")), None);
    }

    #[test]
    fn a_multi_segment_path_is_not_counted() {
        // Not a single short-link segment, so it cannot be a link id.
        assert_eq!(classify(&record("GET", "302", "/foo/bar")), None);
    }

    // The following prove the tightened `^/[a-z0-9]{7}$` rule: internet scanner probes
    // observed in production get a 302 to the SPA but must classify as None so they
    // never reach the `attribute_exists(LinkId)` increment and never trip the alarm.

    #[test]
    fn a_dotted_probe_is_not_counted() {
        // '/compose.yaml' has a dot and wrong length; the dot alone excludes it.
        assert_eq!(classify(&record("GET", "302", "/compose.yaml")), None);
    }

    #[test]
    fn a_json_probe_is_not_counted() {
        assert_eq!(classify(&record("GET", "302", "/stripe-keys.json")), None);
    }

    #[test]
    fn an_underscore_probe_is_not_counted() {
        // '/nginx_status': underscore is not in [a-z0-9], so it is rejected on shape.
        assert_eq!(classify(&record("GET", "302", "/nginx_status")), None);
    }

    #[test]
    fn a_multi_dot_probe_is_not_counted() {
        assert_eq!(
            classify(&record("GET", "302", "/terraform.tfstate.backup")),
            None
        );
    }

    #[test]
    fn a_wrong_length_alphanumeric_path_is_not_counted() {
        // Six chars: too short for a cuid2 id even though every char is in [a-z0-9].
        assert_eq!(classify(&record("GET", "302", "/abc123")), None);
        // Eight chars: too long.
        assert_eq!(classify(&record("GET", "302", "/abcd1234")), None);
    }

    #[test]
    fn an_uppercase_path_is_not_counted() {
        // cuid2 output is lowercase only; an uppercase segment is not a valid id shape.
        assert_eq!(classify(&record("GET", "302", "/Abc1234")), None);
    }

    #[test]
    fn parse_skips_headers_and_malformed_lines_without_panicking() {
        // A realistic mix: a version/fields header, a blank line, a truncated line,
        // and one good record. Only the good record survives.
        let object = concat!(
            "#Version: 1.0\n",
            "#Fields: timestamp(ms) c-country cs-method cs-uri-stem sc-status\n",
            "\n",
            "{\"cs-method\":\"GET\",\"sc-status\":\"302\",\"cs-uri-str",
            "\n",
            "{\"cs-method\":\"GET\",\"sc-status\":\"302\",\"cs-uri-stem\":\"/k3m9x2p\",\"timestamp(ms)\":\"1739035776180\",\"c-country\":\"US\"}\n",
        );
        assert_eq!(parse_log_object(object), vec!["k3m9x2p".to_string()]);
    }

    #[test]
    fn parse_collects_only_countable_hits_from_a_mixed_object() {
        let object = concat!(
            "{\"cs-method\":\"GET\",\"sc-status\":\"302\",\"cs-uri-stem\":\"/aaaaaaa\",\"timestamp(ms)\":\"1\",\"c-country\":\"US\"}\n",
            "{\"cs-method\":\"GET\",\"sc-status\":\"200\",\"cs-uri-stem\":\"/index.html\",\"timestamp(ms)\":\"2\",\"c-country\":\"US\"}\n",
            "{\"cs-method\":\"GET\",\"sc-status\":\"302\",\"cs-uri-stem\":\"/api/links\",\"timestamp(ms)\":\"3\",\"c-country\":\"US\"}\n",
            "{\"cs-method\":\"GET\",\"sc-status\":\"302\",\"cs-uri-stem\":\"/bbbbbbb\",\"timestamp(ms)\":\"4\",\"c-country\":\"US\"}\n",
        );
        assert_eq!(
            parse_log_object(object),
            vec!["aaaaaaa".to_string(), "bbbbbbb".to_string()]
        );
    }

    #[test]
    fn aggregate_counts_hits_per_distinct_link() {
        let mut link_ids = vec!["a".to_string(); 50];
        link_ids.extend(vec!["b".to_string(); 3]);

        let counts = aggregate(&link_ids);
        assert_eq!(counts.get("a"), Some(&50));
        assert_eq!(counts.get("b"), Some(&3));
        assert_eq!(counts.len(), 2);
    }

    /// FR-12.4: processing the same object twice must increment each link exactly
    /// once. The first delivery wins the claim (`claimed = true`) and plans the full
    /// aggregate; the redelivery loses the claim (`claimed = false`) and plans NOTHING,
    /// so no second increment is ever issued.
    #[test]
    fn an_already_claimed_object_plans_zero_increments() {
        let link_ids = vec!["k120oizr".to_string(), "k120oizr".to_string()];

        let first = plan_increments(true, &link_ids);
        assert_eq!(first.get("k120oizr"), Some(&2), "first delivery counts the hits");

        let redelivery = plan_increments(false, &link_ids);
        assert!(
            redelivery.is_empty(),
            "a redelivered, already-claimed object must plan no increments"
        );
    }
}
