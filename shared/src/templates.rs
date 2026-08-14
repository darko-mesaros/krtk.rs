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
}
