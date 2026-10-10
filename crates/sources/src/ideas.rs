//! Topic ideas: Google Trends daily searches (RSS, keyless) and news headlines.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util::{REQUEST_TIMEOUT, clean_text, endpoint, http_url, read_fresh, text_response};
use crate::{SearchCategory, Sources};

const TRENDS_RSS: &str = "https://trends.google.com/trending/rss";
const TRENDS_TTL: Duration = Duration::from_secs(30 * 60);
const TITLE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaSource {
    Trends,
    News,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Idea {
    pub title: String,
    /// "google_trends" or the search engine
    pub source: String,
    pub url: Option<String>,
    /// Approximate searches ("200K+") or a news headline/snippet.
    pub detail: Option<String>,
}

impl Sources {
    pub(crate) async fn ideas_impl(&self, source: IdeaSource, query: Option<&str>, geo: &str, limit: usize) -> anyhow::Result<Vec<Idea>> {
        let limit = limit.clamp(1, 50);
        let ideas = match source {
            IdeaSource::Trends => parse_trends_rss(&self.trends_rss(&trends_geo(geo)?).await?),
            IdeaSource::News => {
                let query = query.map(str::trim).filter(|q| !q.is_empty()).unwrap_or("top news today");
                self.web_search(query, SearchCategory::News, limit).await?.into_iter().map(|h| Idea {
                    title: h.title,
                    source: h.engine,
                    url: Some(h.url),
                    detail: Some(clean_text(&h.snippet, 200)).filter(|s| !s.is_empty()),
                }).collect()
            }
        };
        Ok(finish(ideas, limit))
    }

    /// Raw daily-trends RSS for `geo`, cached per geo for 30 minutes.
    async fn trends_rss(&self, geo: &str) -> anyhow::Result<String> {
        let cache = self.paths.cache().join("ideas").join(format!("trends-{geo}.xml"));
        if let Some(xml) = read_fresh(&cache, TRENDS_TTL) && !parse_trends_rss(&xml).is_empty() {
            return Ok(xml);
        }
        let resp = self.http.get(endpoint("google_trends", TRENDS_RSS)).query(&[("geo", geo)]).timeout(REQUEST_TIMEOUT).send().await?;
        let xml = text_response("google_trends", resp).await?;
        // A consent/error page must not be cached (or shown as "no ideas").
        anyhow::ensure!(!parse_trends_rss(&xml).is_empty(), "Google Trends returned no trending searches for {geo}");
        if let Err(e) = clipcut_core::model::write_atomic(&cache, xml.as_bytes()) {
            tracing::debug!("google trends cache write failed: {e:#}");
        }
        Ok(xml)
    }
}

/// Two ASCII letters, uppercased; empty means "US".
fn trends_geo(geo: &str) -> anyhow::Result<String> {
    let geo = geo.trim();
    if geo.is_empty() { return Ok("US".into()); }
    anyhow::ensure!(geo.len() == 2 && geo.bytes().all(|b| b.is_ascii_alphabetic()),
        "geo must be a 2-letter country code like \"US\"; got {:?}", clean_text(geo, 20));
    Ok(geo.to_ascii_uppercase())
}

/// Clean titles (≤200 chars), keep HTTP(S) URLs only, drop empty and
/// case-insensitive duplicate titles, then take `limit`.
fn finish(ideas: Vec<Idea>, limit: usize) -> Vec<Idea> {
    let mut seen = HashSet::new();
    ideas.into_iter().filter_map(|mut i| {
        i.title = clean_text(&i.title, TITLE_CHARS);
        i.url = i.url.filter(|u| http_url(u));
        (!i.title.is_empty() && seen.insert(i.title.to_lowercase())).then_some(i)
    }).take(limit).collect()
}

/// Google Trends daily RSS (`trends.google.com/trending/rss?geo=XX`) to ideas:
/// `<title>`, `<ht:approx_traffic>` + first `<ht:news_item_title>` as detail,
/// first `<ht:news_item_url>` (else `<link>`) as URL.
pub fn parse_trends_rss(xml: &str) -> Vec<Idea> {
    let mut out = Vec::new();
    for item in elements(xml, "item") {
        let Some(title) = first_text(item, "title").map(|t| clean_text(&t, TITLE_CHARS)).filter(|t| !t.is_empty()) else {
            continue;
        };
        let traffic = first_text(item, "ht:approx_traffic").map(|t| format!("{} searches", clean_text(&t, 40)));
        let headline = first_text(item, "ht:news_item_title").map(|h| clean_text(&h, 200));
        let detail = match (traffic, headline) {
            (Some(t), Some(h)) => Some(format!("{t} · {h}")),
            (t, h) => t.or(h),
        };
        let url = first_text(item, "ht:news_item_url").filter(|u| http_url(u))
            .or_else(|| first_text(item, "link").filter(|u| http_url(u)));
        out.push(Idea { title, source: "google_trends".into(), url, detail });
    }
    out
}

/// Raw inner XML of every `<name ...>...</name>` element, in document order.
/// `name` must match exactly (`<title` never matches `<ht:news_item_title>`).
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        if !rest.starts_with(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace()) { continue; }
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') { rest = &rest[gt + 1..]; continue; }
        let body = &rest[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    out
}

/// First non-empty decoded text of `<name>` inside `block`.
fn first_text(block: &str, name: &str) -> Option<String> {
    elements(block, name).into_iter().map(xml_text).find(|s| !s.is_empty())
}

/// Element text: CDATA kept verbatim, entities decoded outside it, whitespace collapsed.
fn xml_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find("<![CDATA[") {
        out.push_str(&decode_entities(&rest[..i]));
        let body = &rest[i + "<![CDATA[".len()..];
        let end = body.find("]]>").unwrap_or(body.len());
        out.push_str(&body[..end]);
        rest = body.get(end + 3..).unwrap_or("");
    }
    out.push_str(&decode_entities(rest));
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// XML's predefined entities plus numeric references (`&#39;`, `&#x2019;`).
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.bytes().skip(1).take(12).position(|b| b == b';').and_then(|n| {
            let c = match &rest[1..1 + n] {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                name => {
                    let num = name.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code).filter(|c| *c != '\0')?
                }
            };
            Some((c, n + 2))
        });
        match decoded {
            Some((c, len)) => { out.push(c); rest = &rest[len..]; }
            None => { out.push('&'); rest = &rest[1..]; }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss xmlns:atom="http://www.w3.org/2005/Atom" xmlns:ht="https://trends.google.com/trending/rss" version="2.0">
  <channel>
    <title>Daily Search Trends</title>
    <link>https://trends.google.com/trending/rss?geo=US</link>
    <atom:link href="https://trends.google.com/trending/rss?geo=US" rel="self" type="application/rss+xml"/>
    <item>
      <title>kawhi leonard</title>
      <ht:approx_traffic>200K+</ht:approx_traffic>
      <description></description>
      <link>https://trends.google.com/trending/rss?geo=US</link>
      <pubDate>Tue, 22 Oct 2024 04:40:00 -0700</pubDate>
      <ht:picture>https://encrypted-tbn1.gstatic.com/images?q=tbn:x</ht:picture>
      <ht:news_item>
        <ht:news_item_title>Clippers&#39; Kawhi Leonard &amp; the  &quot;knee&quot; update</ht:news_item_title>
        <ht:news_item_url>https://clutchpoints.com/nba/kawhi</ht:news_item_url>
        <ht:news_item_source>ClutchPoints</ht:news_item_source>
      </ht:news_item>
      <ht:news_item>
        <ht:news_item_title>Second headline</ht:news_item_title>
        <ht:news_item_url>https://example.com/second</ht:news_item_url>
      </ht:news_item>
    </item>
    <item>
      <title><![CDATA[Tom & Jerry <reboot>]]></title>
      <ht:approx_traffic>
        5,000+
      </ht:approx_traffic>
      <link>javascript:alert(1)</link>
    </item>
    <item><title>   </title><ht:approx_traffic>100+</ht:approx_traffic></item>
    <item>
      <title>caf&#xE9; &unknown; R&D</title>
      <ht:news_item><ht:news_item_title>Only a headline</ht:news_item_title><ht:news_item_url>file:///etc/passwd</ht:news_item_url></ht:news_item>
      <link>https://trends.google.com/trending/rss?geo=US</link>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_trends_rss() {
        let ideas = parse_trends_rss(RSS);
        assert_eq!(ideas.len(), 3, "{ideas:?}");
        assert_eq!(ideas[0].title, "kawhi leonard");
        assert_eq!(ideas[0].source, "google_trends");
        assert_eq!(ideas[0].detail.as_deref(), Some("200K+ searches · Clippers' Kawhi Leonard & the \"knee\" update"));
        assert_eq!(ideas[0].url.as_deref(), Some("https://clutchpoints.com/nba/kawhi"));
        assert_eq!(ideas[1].title, "Tom & Jerry <reboot>");
        assert_eq!(ideas[1].detail.as_deref(), Some("5,000+ searches"));
        assert_eq!(ideas[1].url, None);
        assert_eq!(ideas[2].title, "café &unknown; R&D");
        assert_eq!(ideas[2].detail.as_deref(), Some("Only a headline"));
        assert_eq!(ideas[2].url.as_deref(), Some("https://trends.google.com/trending/rss?geo=US"));
        assert!(parse_trends_rss("<html>consent</html>").is_empty());
        assert!(parse_trends_rss("<item><title>unterminated").is_empty());
    }

    #[test]
    fn finishes_and_validates_geo() {
        let idea = |t: &str, u: &str| Idea { title: t.into(), source: "x".into(), url: Some(u.into()), detail: None };
        let out = finish(vec![idea("  Moon  landing ", "https://a.com"), idea("moon LANDING", "https://b.com"), idea(" ", "https://c.com"),
            idea(&"z".repeat(500), "file:///x"), idea("Mars", "https://d.com")], 2);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].title.as_str(), out[0].url.as_deref()), ("Moon landing", Some("https://a.com")));
        assert_eq!((out[1].title.chars().count(), out[1].url.as_deref()), (200, None));
        assert_eq!(trends_geo("").unwrap(), "US");
        assert_eq!(trends_geo(" gb ").unwrap(), "GB");
        for bad in ["USA", "u", "1A", "../", "ÜS"] { assert!(trends_geo(bad).is_err(), "{bad}"); }
    }
}
