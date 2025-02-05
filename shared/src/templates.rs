pub use askama::Template;
use serde::Deserialize; 

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

#[derive(Template, Debug)]
#[template(path = "new_short_link.html")]
pub struct NewShortLink {
    pub link: String,
    pub domain: &'static str,
}
