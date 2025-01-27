use reqwest::{Client, Url};
use scraper::{selector::Selector, Html};

#[derive(Default, Debug)]
pub struct UrlDetails {
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug)]
pub struct UrlInfo {
    pub http_client: Client,
}

impl UrlInfo {
    pub fn new(http_client: Client) -> Self {
        Self { http_client }
    }

    pub async fn fetch_details(&self, url: &str) -> Result<UrlDetails, String> {
        // Making a GET request
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Cannot scrape '{}': {}", url, e))?;

        // Getting the content-type from the page
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok()) // Convert to Option<&str>
            .map(|h| h.chars().take(32).collect::<String>()); // limit to 32 chars
                                                              // and return Option<String>

        let mut title = None;
        let mut description = None;
        let mut image = None;

        // If the content-type starts with "text/html" we proceed
        //
        // This matches! macro is basically doing:
        // if let Some(ct) = &content_type {
        //   if ct.starts_with("text/html") {
        //     // do something
        //   }
        // }
        if matches!(content_type, Some(ref ct) if ct.starts_with("text/html")) {
            // Test if we get some response from .text()
            if let Ok(html_body) = response.text().await {
                // Parse the document
                let document = Html::parse_document(&html_body);

                // See if we can parse the title - we are using .next() as the .select() returns an
                // iterator. It stops iterating after finding the match
                // Parse the TITLE
                if let Some(title_element) = document
                    .select(&Selector::parse("head > title").unwrap())
                    .next()
                {
                    title = Some(
                        // Get the inner html, trim the whitespace, and limit it to 256 chars
                        title_element
                            .inner_html()
                            .trim()
                            .chars()
                            .take(256)
                            .collect::<String>(),
                    );
                }
                // Parse the description
                if let Some(description_element) = document
                    .select(&Selector::parse("head > meta[name=description]").unwrap())
                    .next()
                {
                    description = description_element
                        // Get the value of its specific attribute ("content"), and limit it to
                        // 256 chars
                        .value()
                        .attr("content")
                        .map(|s| s.chars().take(256).collect::<String>());
                }
                // Parse the Image
                if let Some(image_element) = document
                    .select(&Selector::parse("head > meta[property='og:image']").unwrap())
                    .next()
                {
                    image = image_element
                        // Get the value of its specific attribute ("content"), and limit it to
                        // 512 chars
                        .value()
                        .attr("content")
                        .map(|s| s.chars().take(512).collect::<String>());
                // Cant find anything in OpenGraph - lets try the first image we find in the <body>
                } else if let Some(image_element) = document
                    .select(&Selector::parse("body * img").unwrap()) // TODO: Handle unwrap
                    .next()
                {
                    // The image in the <img src= > can sometimes be relative (ie just a filename)
                    // So let's make sure we include the full url in it:
                    let base_url = Url::parse(url).unwrap(); // TODO: Handle unwrap

                    if let Some(image_path) = image_element.value().attr("src") {
                        if let Ok(image_url) = base_url.join(image_path) {
                            image = Some(image_url.to_string());
                        }
                    }
                }
            }
        }
        Ok(UrlDetails {
            content_type,
            title,
            description,
            image,
        })
    }
}
