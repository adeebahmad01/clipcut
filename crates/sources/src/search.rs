//! Web search backends: SearXNG (self-hosted, JSON), Brave and Tavily (keyed),
//! DuckDuckGo HTML + its vqd JSON endpoints (keyless).

use std::sync::LazyLock;
use std::time::Duration;

use crate::util::{bounded, check_mock_key, endpoint, http_url, json_response, text_response, REQUEST_TIMEOUT};
use regex::Regex;
use serde_json::Value;

use crate::{SearchCategory, SearchHit, Sources};

fn searxng_category(c: SearchCategory) -> &'static str {
    match c {
        SearchCategory::General => "general",
        SearchCategory::News => "news",
        SearchCategory::Images => "images",
        SearchCategory::Videos => "videos",
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Make protocol-relative / relative image URLs absolute.
fn absolutize(u: Option<String>, base: &str) -> Option<String> {
    let u = u?;
    if u.starts_with("//") {
        return Some(format!("https:{u}")).filter(|u| http_url(u));
    }
    if u.starts_with("http://") || u.starts_with("https://") {
        return Some(u).filter(|u| http_url(u));
    }
    url::Url::parse(base).ok()?.join(&u).ok().map(|x| x.to_string()).filter(|u| http_url(u))
}

pub(crate) fn parse_searxng(v: &Value, base: &str, category: SearchCategory) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for r in v["results"].as_array().into_iter().flatten() {
        let Some(url) = absolutize(str_of(r, "url"), base) else { continue };
        let image_url = absolutize(str_of(r, "img_src"), base);
        if category == SearchCategory::Images && image_url.is_none() {
            continue;
        }
        let thumb = absolutize(str_of(r, "thumbnail_src").or_else(|| str_of(r, "thumbnail")), base);
        let mut snippet = str_of(r, "content").unwrap_or_default();
        if let Some(res) = str_of(r, "resolution") {
            // Keep "1920x1080"-style resolution where image consumers can read it.
            if snippet.is_empty() {
                snippet = res;
            } else {
                snippet = format!("{snippet} [{res}]");
            }
        }
        out.push(SearchHit {
            title: str_of(r, "title").unwrap_or_else(|| url.clone()),
            url,
            snippet,
            image_url,
            thumbnail_url: thumb,
            published: str_of(r, "publishedDate"),
            engine: "searxng".into(),
        });
    }
    out
}

impl Sources {
    pub(crate) fn searxng_base(&self) -> String {
        crate::util::mock_base().map(|base| format!("{base}/sources-mock/searxng"))
            .unwrap_or_else(|| self.config.search.searxng_url.trim_end_matches('/').to_string())
    }

    pub(crate) async fn searxng(&self, query: &str, category: SearchCategory) -> anyhow::Result<Vec<SearchHit>> {
        let base = self.searxng_base();
        anyhow::ensure!(http_url(&base), "SearXNG HTTP(S) URL not configured");
        let resp = self
            .http
            .get(format!("{base}/search"))
            .query(&[
                ("q", query),
                ("format", "json"),
                ("categories", searxng_category(category)),
                ("language", "en"),
                ("safesearch", "1"),
            ])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let v = json_response("searxng", resp).await?;
        Ok(parse_searxng(&v, &base, category))
    }

    async fn brave(&self, query: &str, category: SearchCategory, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let path = match category {
            SearchCategory::General => "web",
            SearchCategory::News => "news",
            SearchCategory::Images => "images",
            SearchCategory::Videos => "videos",
        };
        let count = limit.clamp(1, 20).to_string();
        let v: Value = self
            .keys
            .with_key("brave", |key| {
                let req = self
                    .http
                    .get(endpoint("brave", &format!("https://api.search.brave.com/res/v1/{path}/search")))
                    .query(&[("q", query), ("count", count.as_str())])
                    .timeout(REQUEST_TIMEOUT)
                    .header("X-Subscription-Token", &key)
                    .header("Accept", "application/json");
                async move { check_mock_key(&key)?; json_response("brave", req.send().await?).await }
            })
            .await?;
        let list = if category == SearchCategory::General { &v["web"]["results"] } else { &v["results"] };
        let mut out = Vec::new();
        for r in list.as_array().into_iter().flatten() {
            let Some(url) = str_of(r, "url") else { continue };
            let image_url = r["properties"]["url"].as_str().map(str::to_string);
            if category == SearchCategory::Images && image_url.is_none() {
                continue;
            }
            out.push(SearchHit {
                title: str_of(r, "title").unwrap_or_else(|| url.clone()),
                url,
                snippet: str_of(r, "description").unwrap_or_default(),
                image_url,
                thumbnail_url: r["thumbnail"]["src"].as_str().map(str::to_string),
                published: str_of(r, "age").or_else(|| str_of(r, "page_age")),
                engine: "brave".into(),
            });
        }
        Ok(out)
    }

    async fn tavily(&self, query: &str, category: SearchCategory, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let images = category == SearchCategory::Images;
        let body = serde_json::json!({
            "query": query,
            "max_results": limit.clamp(1, 20),
            "topic": if category == SearchCategory::News { "news" } else { "general" },
            "include_images": images,
        });
        let v: Value = self
            .keys
            .with_key("tavily", |key| {
                let req = self.http.post(endpoint("tavily", "https://api.tavily.com/search")).timeout(REQUEST_TIMEOUT).bearer_auth(&key).json(&body);
                async move { check_mock_key(&key)?; json_response("tavily", req.send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        if images {
            for img in v["images"].as_array().into_iter().flatten() {
                let url = img.as_str().map(str::to_string).or_else(|| str_of(img, "url"));
                let Some(url) = url else { continue };
                out.push(SearchHit {
                    title: str_of(img, "description").unwrap_or_else(|| query.to_string()),
                    url: url.clone(),
                    snippet: String::new(),
                    image_url: Some(url),
                    thumbnail_url: None,
                    published: None,
                    engine: "tavily".into(),
                });
            }
            return Ok(out);
        }
        for r in v["results"].as_array().into_iter().flatten() {
            let Some(url) = str_of(r, "url") else { continue };
            out.push(SearchHit {
                title: str_of(r, "title").unwrap_or_else(|| url.clone()),
                url,
                snippet: str_of(r, "content").unwrap_or_default().chars().take(400).collect(),
                image_url: None,
                thumbnail_url: None,
                published: str_of(r, "published_date"),
                engine: "tavily".into(),
            });
        }
        Ok(out)
    }

    async fn ddg_html(&self, query: &str) -> anyhow::Result<Vec<SearchHit>> {
        let resp = self
            .http
            .post(endpoint("duckduckgo", "https://html.duckduckgo.com/html/"))
            .form(&[("q", query), ("kl", "us-en")])
            .header("Referer", "https://html.duckduckgo.com/")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let html = text_response("duckduckgo", resp).await?;
        let hits = parse_ddg_html(&html);
        if hits.is_empty() && html.contains("anomaly") {
            anyhow::bail!("DuckDuckGo rate-limited this IP (bot challenge)");
        }
        Ok(hits)
    }

    /// DuckDuckGo's JSON endpoints need a per-query `vqd` token.
    async fn ddg_vqd(&self, query: &str, ia: &str) -> anyhow::Result<String> {
        static VQD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"vqd=["']?([\d-]+)["']?"#).unwrap());
        let resp = self
            .http
            .get(endpoint("duckduckgo", "https://duckduckgo.com/"))
            .query(&[("q", query), ("ia", ia), ("iax", ia)])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let html = text_response("duckduckgo", resp).await?;
        VQD.captures(&html)
            .map(|c| c[1].to_string())
            .ok_or_else(|| anyhow::anyhow!("DuckDuckGo vqd token not found"))
    }

    async fn ddg_json(&self, query: &str, endpoint: &str, ia: &str, extra: &[(&str, &str)]) -> anyhow::Result<Value> {
        let vqd = self.ddg_vqd(query, ia).await?;
        let mut params: Vec<(&str, &str)> = vec![("l", "us-en"), ("o", "json"), ("q", query), ("vqd", &vqd), ("p", "1")];
        params.extend_from_slice(extra);
        let resp = self
            .http
            .get(crate::util::endpoint("duckduckgo", &format!("https://duckduckgo.com/{endpoint}")))
            .query(&params)
            .header("Referer", "https://duckduckgo.com/")
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        json_response("duckduckgo", resp).await
    }

    /// DuckDuckGo image search (keyless). `gif_only` restricts to animated GIFs.
    pub(crate) async fn ddg_images(&self, query: &str, gif_only: bool) -> anyhow::Result<Vec<SearchHit>> {
        let filter = if gif_only { ",,,type:gif,," } else { ",,,,," };
        let v = self.ddg_json(query, "i.js", "images", &[("f", filter)]).await?;
        let mut out = Vec::new();
        for r in v["results"].as_array().into_iter().flatten() {
            let Some(image) = str_of(r, "image") else { continue };
            let (w, h) = (r["width"].as_u64().unwrap_or(0), r["height"].as_u64().unwrap_or(0));
            out.push(SearchHit {
                title: str_of(r, "title").unwrap_or_default(),
                url: str_of(r, "url").unwrap_or_else(|| image.clone()),
                snippet: if w > 0 { format!("[{w}x{h}]") } else { String::new() },
                image_url: Some(image),
                thumbnail_url: str_of(r, "thumbnail"),
                published: None,
                engine: "duckduckgo".into(),
            });
        }
        Ok(out)
    }

    async fn ddg_news(&self, query: &str) -> anyhow::Result<Vec<SearchHit>> {
        let v = self.ddg_json(query, "news.js", "news", &[("noamp", "1")]).await?;
        let mut out = Vec::new();
        for r in v["results"].as_array().into_iter().flatten() {
            let Some(url) = str_of(r, "url") else { continue };
            out.push(SearchHit {
                title: str_of(r, "title").unwrap_or_else(|| url.clone()),
                url,
                snippet: str_of(r, "excerpt").map(|s| strip_html(&s)).unwrap_or_default(),
                image_url: str_of(r, "image"),
                thumbnail_url: str_of(r, "image"),
                published: r["date"].as_i64().map(|d| d.to_string()),
                engine: "duckduckgo".into(),
            });
        }
        Ok(out)
    }

    async fn ddg_videos(&self, query: &str) -> anyhow::Result<Vec<SearchHit>> {
        let v = self.ddg_json(query, "v.js", "videos", &[]).await?;
        let mut out = Vec::new();
        for r in v["results"].as_array().into_iter().flatten() {
            let Some(url) = str_of(r, "content") else { continue };
            out.push(SearchHit {
                title: str_of(r, "title").unwrap_or_else(|| url.clone()),
                url,
                snippet: str_of(r, "description").unwrap_or_default(),
                image_url: None,
                thumbnail_url: r["images"]["medium"].as_str().map(str::to_string),
                published: str_of(r, "published"),
                engine: "duckduckgo".into(),
            });
        }
        Ok(out)
    }

    pub(crate) async fn duckduckgo(&self, query: &str, category: SearchCategory) -> anyhow::Result<Vec<SearchHit>> {
        match category {
            SearchCategory::General => self.ddg_html(query).await,
            SearchCategory::Images => self.ddg_images(query, false).await,
            SearchCategory::News => match self.ddg_news(query).await {
                Ok(h) if !h.is_empty() => Ok(h),
                _ => self.ddg_html(&format!("{query} news")).await,
            },
            SearchCategory::Videos => self.ddg_videos(query).await,
        }
    }

    pub(crate) async fn web_search_impl(
        &self,
        query: &str,
        category: SearchCategory,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        if limit == 0 || query.trim().is_empty() { return Ok(Vec::new()); }
        anyhow::ensure!(query.len() <= 2000, "search query exceeds 2000 bytes");
        let limit = limit.min(30);
        let mut errors = Vec::new();
        let mut any_ok = false;
        // Futures are polled sequentially: optional keyed APIs never run when
        // an earlier free provider supplied usable hits.
        type SearchFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<SearchHit>>> + Send + 'a>>;
        let mut stages: Vec<(&str, SearchFuture<'_>)> = vec![("searxng", Box::pin(self.searxng(query, category)))];
        if self.config.search.duckduckgo_fallback {
            stages.push(("duckduckgo", Box::pin(self.duckduckgo(query, category))));
        }
        if self.keys.has("brave") { stages.push(("brave", Box::pin(self.brave(query, category, limit)))); }
        if self.keys.has("tavily") && category != SearchCategory::Videos {
            stages.push(("tavily", Box::pin(self.tavily(query, category, limit))));
        }
        for (name, future) in stages {
            match bounded(name, Duration::from_secs(18), future).await {
                Ok(hits) => {
                    any_ok = true;
                    let hits = dedupe(hits, category, limit);
                    if !hits.is_empty() { return Ok(hits); }
                }
                Err(e) => {
                    tracing::debug!(provider = name, "search backend failed: {e:#}");
                    errors.push(format!("{name}: {e:#}"));
                }
            }
        }
        if any_ok { return Ok(Vec::new()); }
        anyhow::bail!("web search failed: {}", errors.join("; "))
    }
}

fn dedupe(hits: Vec<SearchHit>, category: SearchCategory, limit: usize) -> Vec<SearchHit> {
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter_map(|mut h| {
            if !http_url(&h.url) { return None; }
            h.image_url = h.image_url.filter(|u| http_url(u));
            h.thumbnail_url = h.thumbnail_url.filter(|u| http_url(u));
            if category == SearchCategory::Images && h.image_url.is_none() { return None; }
            Some(h)
        })
        .filter(|h| seen.insert(h.image_url.clone().unwrap_or_else(|| h.url.trim_end_matches('/').to_string())))
        .take(limit)
        .collect()
}

fn strip_html(s: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    TAG.replace_all(s, "").replace("&amp;", "&").replace("&#x27;", "'").replace("&quot;", "\"")
}

/// `//duckduckgo.com/l/?uddg=<encoded>&rut=...` -> the real URL.
pub fn unwrap_ddg_url(href: &str) -> Option<String> {
    let abs = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    let parsed = url::Url::parse(&abs).or_else(|_| url::Url::parse("https://duckduckgo.com").and_then(|b| b.join(href))).ok()?;
    if !http_url(parsed.as_str()) { return None; }
    if parsed.host_str().is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com")) {
        let (_, target) = parsed.query_pairs().find(|(k, _)| k == "uddg")?;
        return http_url(&target).then(|| target.into_owned());
    }
    Some(parsed.to_string())
}

/// Parse DuckDuckGo's HTML results page (ads skipped).
pub fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let result = Selector::parse("div.result, div.web-result").unwrap();
    let link = Selector::parse("a.result__a").unwrap();
    let snippet = Selector::parse(".result__snippet").unwrap();
    let mut out = Vec::new();
    for r in doc.select(&result) {
        let classes = r.value().attr("class").unwrap_or_default();
        if classes.contains("result--ad") {
            continue;
        }
        let Some(a) = r.select(&link).next() else { continue };
        let Some(url) = a.value().attr("href").and_then(unwrap_ddg_url) else { continue };
        if url.contains("duckduckgo.com/y.js") {
            continue;
        }
        let title = a.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
        let snip = r
            .select(&snippet)
            .next()
            .map(|s| s.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        out.push(SearchHit {
            title,
            url,
            snippet: snip,
            image_url: None,
            thumbnail_url: None,
            published: None,
            engine: "duckduckgo".into(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"<html><body><div id="links" class="results">
<div class="result results_links results_links_deep result--ad">
  <div class="links_main links_deep result__body"><h2 class="result__title">
  <a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=example.com&amp;u3=x">Buy telescopes</a></h2>
  <a class="result__snippet" href="#">Sponsored</a></div></div>
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fscience.nasa.gov%2Fmission%2Fwebb%2F&amp;rut=abc">James Webb Space <b>Telescope</b> - NASA Science</a>
    </h2>
    <a class="result__snippet" href="//duckduckgo.com/l/?uddg=x">The James Webb Space Telescope is the <b>largest</b>, most powerful telescope.</a>
  </div>
</div>
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://en.wikipedia.org/wiki/James_Webb_Space_Telescope">James Webb Space Telescope - Wikipedia</a></h2>
    <a class="result__snippet">JWST is a space telescope.</a>
  </div>
</div>
</div></body></html>"##;

    #[test]
    fn parses_ddg_html_and_unwraps_redirects() {
        let hits = parse_ddg_html(FIXTURE);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].url, "https://science.nasa.gov/mission/webb/");
        assert_eq!(hits[0].title, "James Webb Space Telescope - NASA Science");
        assert!(hits[0].snippet.starts_with("The James Webb Space Telescope is the largest"));
        assert_eq!(hits[1].url, "https://en.wikipedia.org/wiki/James_Webb_Space_Telescope");
    }

    #[test]
    fn rejects_unsafe_redirects_and_images() {
        assert!(unwrap_ddg_url("//duckduckgo.com/l/?uddg=file%3A%2F%2F%2Fetc%2Fpasswd").is_none());
        assert!(unwrap_ddg_url("javascript:alert(1)").is_none());
        assert_eq!(unwrap_ddg_url("https://evilduckduckgo.com/l/?uddg=https%3A%2F%2Fnasa.gov").unwrap(),
            "https://evilduckduckgo.com/l/?uddg=https%3A%2F%2Fnasa.gov");
        let v = serde_json::json!({"results":[{"url":"file:///tmp/x","img_src":"https://a.com/i.png"},
            {"url":"https://a.com/p","img_src":"javascript:alert(1)"}]});
        assert!(parse_searxng(&v, "https://search.example", SearchCategory::Images).is_empty());
    }

    #[test]
    fn parses_searxng_images() {
        let v = serde_json::json!({"results": [
            {"url": "https://a.com/p", "title": "A", "img_src": "//a.com/i.jpg", "thumbnail_src": "/t.jpg", "resolution": "1920x1080"},
            {"url": "https://b.com/p", "title": "B"}
        ]});
        let hits = parse_searxng(&v, "http://127.0.0.1:8888", SearchCategory::Images);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].image_url.as_deref(), Some("https://a.com/i.jpg"));
        assert_eq!(hits[0].thumbnail_url.as_deref(), Some("http://127.0.0.1:8888/t.jpg"));
        assert!(hits[0].snippet.contains("1920x1080"));
    }
}
