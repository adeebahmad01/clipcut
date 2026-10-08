//! Media providers: Imgflip, GIPHY, Tenor, Wikimedia Commons, Openverse,
//! Pexels, Pixabay, Unsplash, plus web image search.

use std::sync::LazyLock;
use std::time::Duration;

use clipcut_core::config::Format;
use crate::provenance::permissive_license;
use clipcut_core::{Candidate, VisualKind};
use regex::Regex;
use serde_json::{Value, json};

use crate::util::{bounded, check_mock_key, endpoint, http_url, json_response, read_fresh, similarity, text_response, words, REQUEST_TIMEOUT};
use crate::{MemeTemplate, SearchCategory, Sources};

fn cand(kind: VisualKind, provider: &str, title: String, page_url: String) -> Candidate {
    Candidate {
        kind,
        provider: provider.into(),
        title,
        page_url,
        media_url: None,
        thumbnail_url: None,
        duration_s: None,
        width: None,
        height: None,
        start_s: None,
        extra: json!({"license_status": "unknown_requires_review"}),
    }
}

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|x| x.try_into().ok()).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
static RES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{2,5})\s*[x×]\s*(\d{2,5})").unwrap());

fn plain(s: &str) -> String {
    HTML_TAG.replace_all(s, "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Rank templates for a planned template name and/or a free query.
pub fn rank_memes(templates: &[MemeTemplate], wanted: Option<&str>, query: &str, limit: usize) -> Vec<MemeTemplate> {
    let wanted = wanted.map(str::trim).filter(|w| !w.is_empty());
    let mut scored: Vec<(f32, usize)> = templates
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let by_name = wanted.map(|w| similarity(w, &t.name)).unwrap_or(0.0);
            let by_query = if query.trim().is_empty() { 0.0 } else { similarity(query, &t.name) };
            // A good name match always outranks query overlap.
            let score = if by_name >= 0.5 { 10.0 + by_name } else { by_name.max(by_query) };
            // Imgflip lists popular templates first; tiny tie-breaker.
            (score - i as f32 * 1e-4, i)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let has_signal = scored.first().is_some_and(|(s, _)| *s > 0.15);
    scored
        .into_iter()
        .filter(|(s, _)| !has_signal || *s > 0.15)
        .take(limit)
        .map(|(_, i)| templates[i].clone())
        .collect()
}

impl Sources {
    fn vertical(&self) -> bool {
        self.config.video.format == Format::Vertical
    }

    pub(crate) async fn meme_templates_impl(&self) -> anyhow::Result<Vec<MemeTemplate>> {
        let cache = self.paths.cache().join("imgflip.json");
        if let Some(text) = read_fresh(&cache, Duration::from_secs(24 * 3600)) {
            if let Ok(t) = serde_json::from_str::<Vec<MemeTemplate>>(&text) {
                let t = valid_templates(t);
                if !t.is_empty() { return Ok(t); }
            }
        }
        let fetched: anyhow::Result<Vec<MemeTemplate>> = async {
            let v = json_response("imgflip", self.http.get(endpoint("imgflip", "https://api.imgflip.com/get_memes"))
                .timeout(REQUEST_TIMEOUT).send().await?).await?;
            anyhow::ensure!(v["success"].as_bool() == Some(true), "imgflip rejected template request");
            let mut out = Vec::new();
            for m in v["data"]["memes"].as_array().into_iter().flatten() {
                out.push(MemeTemplate {
                    id: m["id"].as_str().map(str::to_string).unwrap_or_else(|| m["id"].to_string()),
                    name: m["name"].as_str().unwrap_or_default().to_string(),
                    url: m["url"].as_str().unwrap_or_default().to_string(),
                    width: u32_of(&m["width"]).unwrap_or(0),
                    height: u32_of(&m["height"]).unwrap_or(0),
                    box_count: u32_of(&m["box_count"]).unwrap_or(2),
                });
            }
            Ok(valid_templates(out))
        }
        .await;
        match fetched {
            Ok(t) => {
                clipcut_core::model::write_atomic(&cache, serde_json::to_string(&t)?.as_bytes())?;
                Ok(t)
            }
            // Stale cache beats nothing.
            Err(e) => read_fresh(&cache, Duration::from_secs(7 * 24 * 3600))
                .and_then(|t| serde_json::from_str(&t).ok()).map(valid_templates)
                .filter(|t| !t.is_empty()).ok_or(e),
        }
    }

    pub(crate) async fn meme_candidates(&self, wanted: Option<&str>, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let templates = self.meme_templates_impl().await?;
        Ok(rank_memes(&templates, wanted, query, limit)
            .into_iter()
            .map(|t| {
                let mut c = cand(VisualKind::Meme, "imgflip", t.name.clone(), format!("https://imgflip.com/meme/{}", t.id));
                c.media_url = Some(t.url.clone());
                c.thumbnail_url = Some(t.url.clone());
                c.width = Some(t.width);
                c.height = Some(t.height);
                c.extra = json!({"template_id": t.id, "box_count": t.box_count, "license_status": "unknown_requires_review"});
                c
            })
            .collect())
    }

    pub(crate) async fn giphy(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 50).to_string();
        let v: Value = self
            .keys
            .with_key("giphy", |key| {
                let req = self.http.get(endpoint("giphy", "https://api.giphy.com/v1/gifs/search")).query(&[
                    ("api_key", key.as_str()),
                    ("q", query),
                    ("limit", n.as_str()),
                    ("rating", "pg-13"),
                    ("lang", "en"),
                ]);
                async move { check_mock_key(&key)?; json_response("giphy", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for g in v["data"].as_array().into_iter().flatten() {
            let imgs = &g["images"];
            let rendition = [&imgs["downsized_small"], &imgs["fixed_height"], &imgs["original"]]
                .into_iter().find(|r| r["mp4"].as_str().is_some_and(http_url)
                    && matches!((u32_of(&r["width"]), u32_of(&r["height"])), (Some(w), Some(h)) if rendition_size(w, h)));
            let Some(r) = rendition else { continue };
            let mut c = cand(
                VisualKind::Gif,
                "giphy",
                g["title"].as_str().unwrap_or("GIF").to_string(),
                g["url"].as_str().unwrap_or_default().to_string(),
            );
            c.media_url = r["mp4"].as_str().map(str::to_string);
            c.thumbnail_url = imgs["fixed_height_still"]["url"].as_str().map(str::to_string);
            c.width = u32_of(&r["width"]);
            c.height = u32_of(&r["height"]);
            c.extra = json!({"id": g["id"], "license_status": "unknown_requires_review"});
            out.push(c);
        }
        Ok(out)
    }

    pub(crate) async fn tenor(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 50).to_string();
        let v: Value = self
            .keys
            .with_key("tenor", |key| {
                let req = self.http.get(endpoint("tenor", "https://tenor.googleapis.com/v2/search")).query(&[
                    ("q", query),
                    ("key", key.as_str()),
                    ("client_key", "clipcut"),
                    ("limit", n.as_str()),
                    ("media_filter", "mp4,gif,tinygif"),
                    ("contentfilter", "medium"),
                    ("locale", "en_US"),
                ]);
                async move { check_mock_key(&key)?; json_response("tenor", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for r in v["results"].as_array().into_iter().flatten() {
            let mp4 = &r["media_formats"]["mp4"];
            let Some(url) = mp4["url"].as_str().filter(|u| http_url(u)) else { continue };
            if !matches!((u32_of(&mp4["dims"][0]), u32_of(&mp4["dims"][1])), (Some(w), Some(h)) if rendition_size(w,h)) { continue; }
            let mut c = cand(
                VisualKind::Gif,
                "tenor",
                r["content_description"].as_str().or(r["title"].as_str()).unwrap_or("GIF").to_string(),
                r["itemurl"].as_str().unwrap_or(url).to_string(),
            );
            c.media_url = Some(url.to_string());
            c.thumbnail_url = r["media_formats"]["tinygif"]["url"].as_str().map(str::to_string);
            c.width = u32_of(&mp4["dims"][0]);
            c.height = u32_of(&mp4["dims"][1]);
            c.duration_s = mp4["duration"].as_f64().map(|d| d as f32);
            c.extra = json!({"id": r["id"], "license_status": "unknown_requires_review"});
            out.push(c);
        }
        Ok(out)
    }

    /// Keyless GIFs: tenor.com search page (server-rendered media URLs), then
    /// image search restricted to animated GIFs.
    pub(crate) async fn keyless_gifs(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        match self.tenor_web(query, limit).await {
            Ok(c) => out.extend(c),
            Err(e) => errors.push(format!("tenor web: {e:#}")),
        }
        if out.len() < limit {
            match self.image_search_hits(&format!("{query} gif"), true).await {
                Ok(hits) => {
                    for h in hits {
                        let Some(img) = h.image_url.clone() else { continue };
                        let path = img.split('?').next().unwrap_or_default().to_lowercase();
                        if !(path.ends_with(".gif") || path.ends_with(".mp4") || path.ends_with(".webm")) {
                            continue;
                        }
                        let mut c = cand(VisualKind::Gif, &h.engine, h.title.clone(), h.url.clone());
                        c.media_url = Some(img);
                        c.thumbnail_url = h.thumbnail_url.clone();
                        if let Some((w, hh)) = parse_res(&h.snippet) {
                            c.width = Some(w);
                            c.height = Some(hh);
                        }
                        out.push(c);
                    }
                }
                Err(e) => errors.push(format!("image search: {e:#}")),
            }
        }
        if out.is_empty() && !errors.is_empty() {
            anyhow::bail!("{}", errors.join("; "));
        }
        out.truncate(limit);
        Ok(out)
    }

    async fn tenor_web(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        static MEDIA: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"https://media\d?\.tenor\.com/(?:m/)?([A-Za-z0-9_-]{6,})/([A-Za-z0-9_.%-]+)\.gif"#).unwrap());
        let slug = words(query).join("-");
        anyhow::ensure!(!slug.is_empty(), "empty query");
        let url = format!("https://tenor.com/search/{slug}-gifs");
        let html = text_response("tenor-web", self.http.get(endpoint("tenor-web", &url)).timeout(REQUEST_TIMEOUT).send().await?).await?;
        // Preserve individual item links when available, rather than claiming
        // the search page is the clip's owner/source.
        let doc = scraper::Html::parse_document(&html);
        let links = scraper::Selector::parse("a[href*=\"/view/\"]").unwrap();
        let images = scraper::Selector::parse("img").unwrap();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for a in doc.select(&links) {
            let Some(page) = a.value().attr("href").and_then(|h| url::Url::parse(&url).ok()?.join(h).ok()) else { continue; };
            let Some(img) = a.select(&images).next() else { continue; };
            let Some(media) = img.value().attr("src").filter(|u| http_url(u)) else { continue; };
            let allowed = crate::util::mock_base().is_some() || url::Url::parse(media).ok()
                .and_then(|u| u.host_str().map(str::to_string)).is_some_and(|h| h.ends_with(".tenor.com"));
            if !allowed || !http_url(page.as_str()) || !seen.insert(media.to_string()) { continue; }
            let mut c = cand(VisualKind::Gif, "tenor", img.value().attr("alt").unwrap_or("GIF").to_string(), page.to_string());
            c.media_url = Some(media.to_string());
            c.thumbnail_url = Some(media.to_string());
            out.push(c);
            if out.len() >= limit { return Ok(out); }
        }
        for m in MEDIA.captures_iter(&html) {
            let id = m[1].to_string();
            // Tenor ids end in a 2-char rendition code; group by the stem.
            let key = id.get(..id.len().saturating_sub(2)).unwrap_or(&id).to_string();
            if !seen.insert(key) {
                continue;
            }
            let name = m[2].replace('-', " ");
            let gif = m[0].to_string();
            let mut c = cand(VisualKind::Gif, "tenor", name.clone(), url.clone());
            c.extra = json!({"source_page_is_search": true, "license_status": "unknown_requires_review"});
            c.media_url = Some(gif.clone());
            c.thumbnail_url = Some(gif);
            out.push(c);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Image search hits: SearXNG images, else DuckDuckGo images.
    pub(crate) async fn image_search_hits(&self, query: &str, gif_only: bool) -> anyhow::Result<Vec<crate::SearchHit>> {
        bounded("free image search", Duration::from_secs(18), async {
            match self.searxng(query, SearchCategory::Images).await {
                Ok(h) if !h.is_empty() => return Ok(h),
                Ok(_) => {}
                Err(e) => tracing::debug!("searxng images: {e:#}"),
            }
            if self.config.search.duckduckgo_fallback { return self.ddg_images(query, gif_only).await; }
            Ok(Vec::new())
        }).await
    }

    pub(crate) async fn web_images(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let hits = self.image_search_hits(query, false).await?;
        Ok(hits
            .into_iter()
            .filter_map(|h| {
                let img = h.image_url.clone()?;
                let mut c = cand(VisualKind::Image, &h.engine, h.title.clone(), h.url.clone());
                c.media_url = Some(img);
                c.thumbnail_url = h.thumbnail_url.clone();
                if let Some((w, hh)) = parse_res(&h.snippet) {
                    c.width = Some(w);
                    c.height = Some(hh);
                }
                Some(c)
            })
            .take(limit * 2)
            .collect())
    }

    async fn commons_query(&self, search: &str, limit: usize, video: bool) -> anyhow::Result<Value> {
        let n = limit.clamp(1, 50).to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("action", "query"),
            ("format", "json"),
            ("formatversion", "2"),
            ("generator", "search"),
            ("gsrsearch", search),
            ("gsrnamespace", "6"),
            ("gsrlimit", &n),
        ];
        if video {
            params.extend([("prop", "videoinfo"), ("viprop", "url|size|mime|extmetadata|derivatives"), ("viextmetadatafilter", "LicenseShortName|LicenseUrl|Artist|Credit|Attribution|AttributionRequired|Copyrighted|Restrictions|ObjectName|ImageDescription")]);
        } else {
            params.extend([("prop", "imageinfo"), ("iiprop", "url|size|mime|extmetadata"), ("iiurlwidth", "1600"), ("iiurlheight", "1600"), ("iiextmetadatafilter", "LicenseShortName|LicenseUrl|Artist|Credit|Attribution|AttributionRequired|Copyrighted|Restrictions|ObjectName|ImageDescription")]);
        }
        let resp = self.http.get(endpoint("wikimedia", "https://commons.wikimedia.org/w/api.php")).query(&params).timeout(REQUEST_TIMEOUT).send().await?;
        json_response("wikimedia", resp).await
    }

    pub(crate) async fn wikimedia_images(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let v = self.commons_query(&format!("{query} filetype:bitmap"), limit, false).await?;
        Ok(commons_candidates(&v, false, limit))
    }

    pub(crate) async fn wikimedia_videos(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let v = self.commons_query(&format!("{query} filetype:video"), limit, true).await?;
        Ok(commons_candidates(&v, true, limit))
    }

    pub(crate) async fn openverse(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 20).to_string();
        let resp = self
            .http
            .get(endpoint("openverse", "https://api.openverse.org/v1/images/"))
            .query(&[("q", query), ("page_size", n.as_str()), ("mature", "false"), ("license", "by,by-sa,cc0,pdm")])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let v = json_response("openverse", resp).await?;
        Ok(openverse_candidates(&v, limit))
    }

    pub(crate) async fn pexels_photos(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 40).to_string();
        let orientation = if self.vertical() { "portrait" } else { "landscape" };
        let v: Value = self
            .keys
            .with_key("pexels", |key| {
                let req = self
                    .http
                    .get(endpoint("pexels", "https://api.pexels.com/v1/search"))
                    .query(&[("query", query), ("per_page", n.as_str()), ("orientation", orientation)])
                    .header("Authorization", &key);
                async move { check_mock_key(&key)?; json_response("pexels", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for p in v["photos"].as_array().into_iter().flatten() {
            let Some(src) = p["src"]["large2x"].as_str().or(p["src"]["original"].as_str()) else { continue };
            let mut c = cand(
                VisualKind::Image,
                "pexels",
                p["alt"].as_str().filter(|s| !s.is_empty()).unwrap_or("Photo").to_string(),
                p["url"].as_str().unwrap_or(src).to_string(),
            );
            c.media_url = Some(src.to_string());
            c.thumbnail_url = p["src"]["medium"].as_str().map(str::to_string);
            c.width = u32_of(&p["width"]);
            c.height = u32_of(&p["height"]);
            c.extra = json!({"author": p["photographer"], "creator_url": p["photographer_url"],
                "license": "Pexels License", "license_url": "https://www.pexels.com/license/", "cleared": false});
            out.push(c);
        }
        Ok(out)
    }

    pub(crate) async fn unsplash(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 30).to_string();
        let orientation = if self.vertical() { "portrait" } else { "landscape" };
        let v: Value = self
            .keys
            .with_key("unsplash", |key| {
                let req = self
                    .http
                    .get(endpoint("unsplash", "https://api.unsplash.com/search/photos"))
                    .query(&[("query", query), ("per_page", n.as_str()), ("orientation", orientation)])
                    .header("Authorization", format!("Client-ID {key}"))
                    .header("Accept-Version", "v1");
                async move { check_mock_key(&key)?; json_response("unsplash", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for p in v["results"].as_array().into_iter().flatten() {
            let Some(src) = p["urls"]["regular"].as_str().or(p["urls"]["full"].as_str()) else { continue };
            let mut c = cand(
                VisualKind::Image,
                "unsplash",
                p["alt_description"].as_str().unwrap_or("Photo").to_string(),
                p["links"]["html"].as_str().unwrap_or(src).to_string(),
            );
            // `regular` is 1080 px wide; scale the reported size to match.
            let (w, h) = (u32_of(&p["width"]).unwrap_or(0), u32_of(&p["height"]).unwrap_or(0));
            if w > 0 {
                c.width = Some(1080);
                c.height = Some((h as f32 * 1080.0 / w as f32) as u32);
            }
            c.media_url = Some(src.to_string());
            c.thumbnail_url = p["urls"]["small"].as_str().map(str::to_string);
            c.extra = json!({"author": p["user"]["name"], "creator_url": p["user"]["links"]["html"],
                "download_tracking_url": p["links"]["download_location"], "api_conditions": "API attribution and download tracking required",
                "license": "Unsplash License", "license_url": "https://unsplash.com/license", "cleared": false});
            out.push(c);
        }
        Ok(out)
    }

    pub(crate) async fn pexels_videos(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(1, 40).to_string();
        let vertical = self.vertical();
        let orientation = if vertical { "portrait" } else { "landscape" };
        let v: Value = self
            .keys
            .with_key("pexels", |key| {
                let req = self
                    .http
                    .get(endpoint("pexels", "https://api.pexels.com/videos/search"))
                    .query(&[("query", query), ("per_page", n.as_str()), ("orientation", orientation)])
                    .header("Authorization", &key);
                async move { check_mock_key(&key)?; json_response("pexels", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for p in v["videos"].as_array().into_iter().flatten() {
            let files: Vec<(u32, u32, String)> = p["video_files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| f["file_type"].as_str() == Some("video/mp4"))
                .filter_map(|f| Some((u32_of(&f["width"])?, u32_of(&f["height"])?, f["link"].as_str()?.to_string())))
                .collect();
            let Some((w, h, link)) = pick_rendition(&files) else { continue };
            let mut c = cand(
                VisualKind::Stock,
                "pexels",
                p["url"].as_str().and_then(|u| u.trim_end_matches('/').rsplit('/').next()).map(|s| s.replace('-', " ")).unwrap_or_else(|| "Video".into()),
                p["url"].as_str().unwrap_or_default().to_string(),
            );
            c.media_url = Some(link);
            c.thumbnail_url = p["image"].as_str().map(str::to_string);
            c.width = Some(w);
            c.height = Some(h);
            c.duration_s = p["duration"].as_f64().map(|d| d as f32);
            c.extra = json!({"author": p["user"]["name"], "creator_url": p["user"]["url"],
                "license": "Pexels License", "license_url": "https://www.pexels.com/license/", "cleared": false});
            out.push(c);
        }
        Ok(out)
    }

    pub(crate) async fn pixabay_videos(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let n = limit.clamp(3, 50).to_string();
        let v: Value = self
            .keys
            .with_key("pixabay", |key| {
                let req = self.http.get(endpoint("pixabay", "https://pixabay.com/api/videos/")).query(&[
                    ("key", key.as_str()),
                    ("q", query),
                    ("per_page", n.as_str()),
                    ("safesearch", "true"),
                ]);
                async move { check_mock_key(&key)?; json_response("pixabay", req.timeout(REQUEST_TIMEOUT).send().await?).await }
            })
            .await?;
        let mut out = Vec::new();
        for p in v["hits"].as_array().into_iter().flatten() {
            let files: Vec<(u32, u32, String)> = ["large", "medium", "small", "tiny"]
                .iter()
                .filter_map(|k| {
                    let f = &p["videos"][*k];
                    let url = f["url"].as_str().filter(|u| !u.is_empty())?;
                    Some((u32_of(&f["width"])?, u32_of(&f["height"])?, url.to_string()))
                })
                .collect();
            let Some((w, h, link)) = pick_rendition(&files) else { continue };
            let mut c = cand(
                VisualKind::Stock,
                "pixabay",
                p["tags"].as_str().unwrap_or("Video").to_string(),
                p["pageURL"].as_str().unwrap_or_default().to_string(),
            );
            c.media_url = Some(link);
            c.thumbnail_url = p["videos"]["tiny"]["thumbnail"].as_str().map(str::to_string);
            c.width = Some(w);
            c.height = Some(h);
            c.duration_s = p["duration"].as_f64().map(|d| d as f32);
            c.extra = json!({"author": p["user"], "user_id": p["user_id"],
                "license": "Pixabay Content License", "license_url": "https://pixabay.com/service/license-summary/", "cleared": false});
            out.push(c);
        }
        Ok(out)
    }
}

fn rendition_size(w: u32, h: u32) -> bool {
    w >= 16 && h >= 16 && w.max(h) <= 1920 && u64::from(w) * u64::from(h) <= 2_073_600
}

/// Largest safely bounded rendition; never fall back to an oversized original.
fn pick_rendition(files: &[(u32, u32, String)]) -> Option<(u32, u32, String)> {
    files.iter().filter(|(w,h,url)| rendition_size(*w,*h) && http_url(url))
        .max_by_key(|(w,h,_)| u64::from(*w) * u64::from(*h)).cloned()
}

fn valid_templates(t: Vec<MemeTemplate>) -> Vec<MemeTemplate> {
    t.into_iter().filter(|t| !t.id.is_empty() && t.id.len() <= 100
        && t.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !t.name.trim().is_empty() && http_url(&t.url) && t.width >= 16 && t.height >= 16
        && t.width.max(t.height) <= 16384 && u64::from(t.width)*u64::from(t.height) <= 40_000_000)
        .take(1000).collect()
}

fn metadata_extra(meta: &Value, mime: &str) -> Value {
    let value = |key: &str| meta[key]["value"].as_str().unwrap_or_default();
    let artist = value("Artist");
    let doc = scraper::Html::parse_fragment(artist);
    let selector = scraper::Selector::parse("a[href]").unwrap();
    let creator_url = doc.select(&selector).filter_map(|e| e.value().attr("href"))
        .find(|u| http_url(u));
    json!({
        "artist": plain(artist), "artist_html": artist, "creator_url": creator_url,
        "license": plain(value("LicenseShortName")), "license_url": value("LicenseUrl"),
        "attribution": plain(value("Attribution")), "credit": plain(value("Credit")),
        "attribution_required": value("AttributionRequired"), "copyrighted": value("Copyrighted"),
        "restrictions": plain(value("Restrictions")), "mime": mime, "cleared": false,
        "description": plain(value("ImageDescription")).chars().take(400).collect::<String>()
    })
}

fn commons_candidates(v: &Value, video: bool, limit: usize) -> Vec<Candidate> {
    let mut pages: Vec<_> = v["query"]["pages"].as_array().into_iter().flatten().collect();
    pages.sort_by_key(|p| p["index"].as_i64().unwrap_or(i64::MAX));
    let mut out = Vec::new();
    for p in pages {
        let info = &p[if video { "videoinfo" } else { "imageinfo" }][0];
        let (Some(orig), Some(page)) = (info["url"].as_str().filter(|u| http_url(u)),
            info["descriptionurl"].as_str().filter(|u| http_url(u))) else { continue; };
        let mime = info["mime"].as_str().unwrap_or_default();
        let extra = metadata_extra(&info["extmetadata"], mime);
        if !permissive_license(extra["license"].as_str().unwrap_or_default(), extra["license_url"].as_str().unwrap_or_default()) { continue; }
        let (media, w, h, duration) = if video {
            if !mime.starts_with("video/") { continue; }
            let Some(duration) = info["duration"].as_f64().filter(|d| d.is_finite() && *d >= 1.0 && *d <= 120.0) else { continue; };
            let derivatives: Vec<_> = info["derivatives"].as_array().into_iter().flatten()
                .filter(|d| d["type"].as_str().is_some_and(|ty| ty.starts_with("video/webm") || ty.starts_with("video/mp4")))
                .filter_map(|d| Some((u32_of(&d["width"])?, u32_of(&d["height"])?, d["src"].as_str()?.to_string())))
                .collect();
            let chosen = pick_rendition(&derivatives).or_else(|| {
                let (w,h) = (u32_of(&info["width"] )?, u32_of(&info["height"])?);
                (rendition_size(w,h) && info["size"].as_u64().is_some_and(|n| n > 0 && n <= 40*1024*1024))
                    .then(|| (w,h,orig.to_string()))
            });
            let Some((w,h,media)) = chosen else { continue; };
            (media, Some(w), Some(h), Some(duration as f32))
        } else {
            if !mime.starts_with("image/") { continue; }
            let thumb = info["thumburl"].as_str().filter(|u| http_url(u));
            let w = u32_of(&info[if thumb.is_some() { "thumbwidth" } else { "width" }]);
            let h = u32_of(&info[if thumb.is_some() { "thumbheight" } else { "height" }]);
            (thumb.unwrap_or(orig).to_string(), w, h, None)
        };
        let mut c = cand(if video { VisualKind::Stock } else { VisualKind::Image }, "wikimedia",
            p["title"].as_str().unwrap_or("Commons media").to_string(), page.to_string());
        c.media_url = Some(media);
        c.thumbnail_url = info["thumburl"].as_str().filter(|u| http_url(u)).map(str::to_string);
        c.width = w; c.height = h; c.duration_s = duration; c.extra = extra;
        out.push(c);
    }
    out.truncate(limit);
    out
}

fn openverse_candidates(v: &Value, limit: usize) -> Vec<Candidate> {
    v["results"].as_array().into_iter().flatten().filter_map(|r| {
        let (url, page) = (r["url"].as_str().filter(|u| http_url(u))?,
            r["foreign_landing_url"].as_str().filter(|u| http_url(u))?);
        let license = format!("{} {}", r["license"].as_str().unwrap_or_default(), r["license_version"].as_str().unwrap_or_default());
        let license_url = r["license_url"].as_str().unwrap_or_default();
        if !permissive_license(&license, license_url) { return None; }
        let mut c = cand(VisualKind::Image, "openverse", r["title"].as_str().unwrap_or("Image").to_string(), page.to_string());
        c.media_url = Some(url.to_string());
        c.thumbnail_url = r["thumbnail"].as_str().filter(|u| http_url(u)).map(str::to_string);
        c.width = u32_of(&r["width"]); c.height = u32_of(&r["height"]);
        c.extra = json!({"creator": r["creator"], "creator_url": r["creator_url"], "license": license.trim(),
            "license_url": license_url, "license_version": r["license_version"], "attribution": r["attribution"],
            "source": r["source"], "filetype": r["filetype"], "cleared": false});
        Some(c)
    }).take(limit).collect()
}

pub(crate) fn parse_res(s: &str) -> Option<(u32, u32)> {
    let m = RES.captures(s)?;
    Some((m[1].parse().ok()?, m[2].parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> MemeTemplate {
        MemeTemplate { id: name.into(), name: name.into(), url: String::new(), width: 500, height: 500, box_count: 2 }
    }

    #[test]
    fn meme_matching() {
        let all: Vec<MemeTemplate> = [
            "Drake Hotline Bling",
            "Two Buttons",
            "Distracted Boyfriend",
            "Change My Mind",
            "UNO Draw 25 Cards",
            "Left Exit 12 Off Ramp",
            "Woman Yelling At Cat",
        ]
        .into_iter()
        .map(t)
        .collect();
        assert_eq!(rank_memes(&all, Some("drake hotline bling"), "", 3)[0].name, "Drake Hotline Bling");
        assert_eq!(rank_memes(&all, Some("Distracted boyfriend meme"), "", 3)[0].name, "Distracted Boyfriend");
        assert_eq!(rank_memes(&all, Some("Uno draw 25"), "", 3)[0].name, "UNO Draw 25 Cards");
        assert_eq!(rank_memes(&all, Some("woman yelling at a cat"), "", 3)[0].name, "Woman Yelling At Cat");
        // No template: rank by query overlap.
        assert_eq!(rank_memes(&all, None, "highway exit ramp choice", 3)[0].name, "Left Exit 12 Off Ramp");
        // Unknown template still returns something.
        assert!(!rank_memes(&all, Some("zzzz"), "qqqq", 3).is_empty());
    }

    #[test]
    fn renditions_cap_at_1080() {
        let files = vec![
            (3840, 2160, "https://example.com/4k".to_string()),
            (1920, 1080, "https://example.com/hd".to_string()),
            (1280, 720, "https://example.com/720".to_string()),
        ];
        assert_eq!(pick_rendition(&files).unwrap().2, "https://example.com/hd");
        let portrait = vec![(2160, 3840, "https://example.com/4k".to_string()), (1080, 1920, "https://example.com/hd".to_string())];
        assert_eq!(pick_rendition(&portrait).unwrap().2, "https://example.com/hd");
    }
}
