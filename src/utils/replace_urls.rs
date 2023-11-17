use std::collections::{HashMap, HashSet};

use anyhow::Context;

lazy_static::lazy_static! {
    static ref URL_REGEX: regex::Regex =
        regex::Regex::new(
            r"(?x)
                \b
                https?://
                (?: [^\s()<>]+
                  | \( [^\s()<>]+ \)
                )+
                (?: [^[:punct:]\s]
                  | \( [^\s()<>]+ \)
                  | /
                )
            ",
        )
        .expect("Failed to compile URL regex");
}

/// Replace URLs with their titles, fetching them from the web.
/// TODO: it's a good idea to use telegram entities rather than regex parsing.
#[allow(clippy::module_name_repetitions)]
pub async fn replace_urls_with_titles(texts: &[&str]) -> Vec<String> {
    let session = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to create reqwest client");

    let link_texts = texts
        .iter()
        .flat_map(|&text| {
            URL_REGEX.find_iter(text).map(|m| m.as_str().to_owned())
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|link| async {
            let title = async {
                let response = session.get(&link).send().await?;
                // TODO: limit request size once reqwest supports it.
                // See https://github.com/seanmonstar/reqwest/issues/1234
                let text = response.text().await?;
                let title = webpage::HTML::from_string(text, None)?
                    .opengraph
                    .properties
                    .get("title")
                    .context("No title")?
                    .clone();
                anyhow::Result::<_>::Ok(title)
            }
            .await
            .unwrap_or_else(|e| {
                log::warn!("Failed to fetch link {link:?}: {e}");
                link.clone()
            });
            (link, title)
        });
    let link_texts = futures::future::join_all(link_texts)
        .await
        .into_iter()
        .collect::<HashMap<_, _>>();

    texts
        .iter()
        .map(|&text| {
            URL_REGEX
                .replace_all(text, |caps: &regex::Captures<'_>| {
                    &link_texts[&caps[0]]
                })
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regex() {
        let urls = [
            "http://example.com",
            "http://example.com/",
            "http://example.com/foo_bar",
            "http://example.com/foo.bar",
            "http://example.com/παράδειγμα",
            "http://example.com/foo_(bar)",
            "http://example.com/foo_(bar)_(baz)",
        ];

        let patterns = ["$", "- $", "- $.", "- $)"];

        for &url in &urls {
            for pattern in &patterns {
                let text = pattern.replace('$', url);
                let result = URL_REGEX
                    .replace_all(&text, |caps: &regex::Captures<'_>| {
                        format!("[{}]", &caps[0])
                    });
                assert_eq!(result, pattern.replace('$', &format!("[{url}]")));
            }
        }
    }
}
