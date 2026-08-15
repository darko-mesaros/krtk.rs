pub use askama::{Template, Error};
use serde::Deserialize; 
use std::fmt::Display;
use chrono::{Utc, TimeZone};

#[derive(Deserialize, Debug)]
pub struct Link {
    title: Option<String>,
    #[serde(rename = "link_id")]
    link_id: String,
    clicks: u32,
    timestamp: i64,
}

#[derive(Template, Debug)]
#[template(path = "links_table.html")]
pub struct LinksTable {
    pub links: Vec<Link>,
    pub domain: &'static str,
    pub has_more: bool,
}

mod filters {
    use super::*;

    #[askama::filter_fn]
    pub fn format_timestamp(ts: &dyn Display, _values: &dyn ::askama::Values) -> ::askama::Result<String> {
        let ts_str = ts.to_string();
        let ts_i64 = ts_str.parse::<i64>()
            .map_err(|_| ::askama::Error::Custom("Invalid timestamp format".into()))?;

        Utc.timestamp_opt(ts_i64, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .ok_or(::askama::Error::Custom("Invalid timestamp".into()))
    }
}

// --- New Link popup

#[derive(Template, Debug)]
#[template(path = "new_short_link.html")]
pub struct NewShortLink {
    pub link: String,
    pub domain: &'static str,
}

// --- Error popup

#[derive(Template, Debug)]
#[template(path = "error_popup.html")]
pub struct ErrorPopup {
    pub message: String,
}

// --- API keys
//
// The key management panel is server-rendered for the same reason the links table is:
// the browser asks for a fragment and swaps it in, so the shape of a key row lives in
// exactly one place instead of being duplicated as an HTML string in JavaScript.
//
// Deliberately no `created_at` here. The list is ordered by it, but the previous
// client-side panel never displayed it, and rendering a field nobody asked for would
// make this refactor a behaviour change.

/// One row in the key list. Never carries the plaintext key -- only the hash is stored,
/// so there is nothing to leak here even by accident.
#[derive(Debug)]
pub struct ApiKeyRow {
    /// The SHA-256 hash, which is also the table's partition key and therefore the id
    /// used in the revoke URL.
    pub key_id: String,
    pub prefix: String,
    pub label: String,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Template, Debug)]
#[template(path = "api_keys_list.html")]
pub struct ApiKeysList {
    pub keys: Vec<ApiKeyRow>,
}

/// The one-time display of a freshly minted key.
#[derive(Template, Debug)]
#[template(path = "new_api_key.html")]
pub struct NewApiKey {
    pub key: String,
    pub label: String,
    pub expires_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `Link` through its `Deserialize` impl, since the fields are private.
    fn link(title: Option<&str>, link_id: &str, clicks: u32, timestamp: i64) -> Link {
        let title_json = match title {
            Some(t) => format!("\"{t}\""),
            None => "null".to_string(),
        };
        serde_json::from_str(&format!(
            r#"{{"title":{title_json},"link_id":"{link_id}","clicks":{clicks},"timestamp":{timestamp}}}"#
        ))
        .expect("Link fixture should deserialize")
    }

    /// The custom-filter signature is the one thing the askama 0.12 -> 0.16 upgrade
    /// actually changed, so pin its rendered output exactly.
    #[test]
    fn format_timestamp_filter_renders_utc() {
        let table = LinksTable {
            links: vec![link(Some("Example"), "abc1234", 42, 1_739_035_776)],
            domain: "krtk.rs/",
            has_more: false,
        };

        let rendered = table.render().expect("LinksTable should render");
        assert!(
            rendered.contains("2025-02-08 17:29:36 UTC"),
            "expected formatted timestamp, got: {rendered}"
        );
    }

    #[test]
    fn links_table_renders_row_content_and_end_marker() {
        let table = LinksTable {
            links: vec![link(Some("Example"), "abc1234", 42, 1_739_035_776)],
            domain: "krtk.rs/",
            has_more: false,
        };

        let rendered = table.render().expect("LinksTable should render");
        assert!(rendered.contains("https://krtk.rs/abc1234"));
        assert!(rendered.contains("Example"));
        assert!(rendered.contains(">42<"));
        // has_more == false must emit the terminal row, not the hx-get pagination row.
        assert!(rendered.contains("All items loaded"));
        assert!(!rendered.contains("hx-trigger=\"revealed\""));
    }

    #[test]
    fn links_table_emits_pagination_row_on_last_link_when_more_remain() {
        let table = LinksTable {
            links: vec![link(None, "zzz9999", 0, 1_739_035_776)],
            domain: "krtk.rs/",
            has_more: true,
        };

        let rendered = table.render().expect("LinksTable should render");
        assert!(rendered.contains("hx-trigger=\"revealed\""));
        assert!(rendered.contains("last_evaluated_id=zzz9999"));
        assert!(!rendered.contains("All items loaded"));
    }

    #[test]
    fn new_short_link_renders_the_full_url() {
        let rendered = NewShortLink { link: "abc1234".to_string(), domain: "krtk.rs/" }
            .render()
            .expect("NewShortLink should render");
        assert!(rendered.contains("krtk.rs/abc1234"));
    }

    #[test]
    fn error_popup_escapes_html_in_the_message() {
        let rendered = ErrorPopup { message: "<script>alert(1)</script>".to_string() }
            .render()
            .expect("ErrorPopup should render");
        // Auto-escaping must remain on for .html templates after the upgrade.
        assert!(!rendered.contains("<script>"));
        assert!(rendered.contains("&#60;script&#62;") || rendered.contains("&lt;script&gt;"));
    }

    // -----------------------------------------------------------------------
    // API key fragments
    // -----------------------------------------------------------------------

    fn key_row(label: &str, last_used_at: Option<i64>, expires_at: Option<i64>) -> ApiKeyRow {
        ApiKeyRow {
            key_id: "a".repeat(64),
            prefix: "krtk_3f9aQ2x".to_string(),
            label: label.to_string(),
            last_used_at,
            expires_at,
        }
    }

    #[test]
    fn key_list_renders_a_revoke_control_addressed_by_key_id() {
        let rendered = ApiKeysList { keys: vec![key_row("laptop CLI", None, None)] }
            .render()
            .expect("ApiKeysList should render");

        // The revoke affordance is a hypermedia control in the response, not a URL the
        // client assembles -- that is the whole point of the refactor.
        assert!(
            rendered.contains(&format!("hx-delete=\"/api/keys/{}\"", "a".repeat(64))),
            "expected an hx-delete control, got: {rendered}"
        );
        assert!(rendered.contains("hx-target=\"#key-list\""));
        assert!(rendered.contains("laptop CLI"));
        assert!(rendered.contains("krtk_3f9aQ2x"));
    }

    #[test]
    fn key_list_renders_absent_timestamps_as_words_not_epoch_zero() {
        // The client-side version fed epoch SECONDS to a millisecond Date constructor and
        // rendered "1/1/1970" for keys with an expiry. Formatting server-side removes the
        // unit ambiguity, so pin both the absent and present cases.
        let rendered = ApiKeysList {
            keys: vec![key_row("no dates", None, None)],
        }
        .render()
        .expect("ApiKeysList should render");
        assert!(rendered.contains("no expiry"));
        assert!(rendered.contains("never used"));
        assert!(!rendered.contains("1970"));

        let dated = ApiKeysList {
            keys: vec![key_row("dated", Some(1_739_035_776), Some(1_739_035_776))],
        }
        .render()
        .expect("ApiKeysList should render");
        assert!(dated.contains("expires 2025-02-08 17:29:36 UTC"), "got: {dated}");
        assert!(dated.contains("last used 2025-02-08 17:29:36 UTC"), "got: {dated}");
    }

    #[test]
    fn empty_key_list_renders_its_own_empty_state() {
        // The empty state has to come from the fragment: the panel swaps this template in
        // wholesale, so a separately-managed empty <p> in the page would never update.
        let rendered = ApiKeysList { keys: vec![] }
            .render()
            .expect("ApiKeysList should render");
        assert!(rendered.contains("No API keys."));
        assert!(!rendered.contains("hx-delete"));
    }

    #[test]
    fn key_list_escapes_a_label_containing_markup() {
        // A label is free text the user chose, and it lands in both element content and an
        // hx-confirm ATTRIBUTE. An unescaped quote in the attribute would end it early.
        let rendered = ApiKeysList {
            keys: vec![key_row("<img src=x onerror=alert(1)>\" evil", None, None)],
        }
        .render()
        .expect("ApiKeysList should render");
        assert!(!rendered.contains("<img"), "label markup was not escaped: {rendered}");
        assert!(
            !rendered.contains("evil\""),
            "a raw quote survived into the attribute: {rendered}"
        );
    }

    #[test]
    fn new_api_key_shows_the_plaintext_and_the_shown_once_warning() {
        let rendered = NewApiKey {
            key: "krtk_abc123".to_string(),
            label: "laptop CLI".to_string(),
            expires_at: Some(1_739_035_776),
        }
        .render()
        .expect("NewApiKey should render");
        assert!(rendered.contains("krtk_abc123"));
        assert!(rendered.contains("will not be shown again"));
        assert!(rendered.contains("expires 2025-02-08 17:29:36 UTC"));
        // The copy control reuses the page's existing helper rather than a second one.
        assert!(rendered.contains("copyToClipboard('krtk_abc123')"));
    }

    #[test]
    fn new_api_key_without_expiry_says_so() {
        let rendered = NewApiKey {
            key: "krtk_abc123".to_string(),
            label: "forever".to_string(),
            expires_at: None,
        }
        .render()
        .expect("NewApiKey should render");
        assert!(rendered.contains("no expiry"));
        assert!(!rendered.contains("1970"));
    }
}
