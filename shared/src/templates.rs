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

// Filters are great: https://rinja-rs.github.io/askama/filters.html
mod filters {
    use super::*;
    pub fn format_timestamp(ts: impl Display) -> ::askama::Result<String> {
        // Convert the Display value to i64
        let ts_str = ts.to_string();
        let ts_i64 = ts_str.parse::<i64>()
            .map_err(|_| Error::Custom("Invalid timestamp format".into()))?;

        Utc.timestamp_opt(ts_i64, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .ok_or(Error::Custom("Invalid timestamp".into()))
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
