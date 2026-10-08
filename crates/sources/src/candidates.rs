//! Per-kind candidate listing across providers.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use clipcut_core::config::Format;
use clipcut_core::{Candidate, VisualKind, VisualPlan};
use serde_json::json;

use crate::util::{bounded, host, http_url};
use crate::{SearchCategory, Sources};

type ProviderFut<'a> = Pin<Box<dyn Future<Output = (&'static str, anyhow::Result<Vec<Candidate>>)> + Send + 'a>>;

fn boxed<'a, F>(name: &'static str, fut: F) -> ProviderFut<'a>
where
    F: Future<Output = anyhow::Result<Vec<Candidate>>> + Send + 'a,
{
    let budget = if name == "youtube" { 60 } else { 24 };
    Box::pin(async move { (name, bounded(name, Duration::from_secs(budget), fut).await) })
}

/// Hosts that are not readable article pages.
const NON_ARTICLE_HOSTS: &[&str] = &[
    "youtube.com", "youtu.be", "m.youtube.com", "tiktok.com", "instagram.com", "x.com", "twitter.com",
    "facebook.com", "fb.com", "i.redd.it", "v.redd.it", "preview.redd.it", "reddit.com", "imgur.com",
    "i.imgur.com", "pinterest.com", "linkedin.com", "threads.net", "vimeo.com", "dailymotion.com",
    "giphy.com", "tenor.com", "amazon.com", "ebay.com", "play.google.com", "apps.apple.com", "spotify.com",
    "duckduckgo.com", "google.com", "bing.com",
];

pub(crate) fn is_article_url(url: &str) -> bool {
    if !http_url(url) {
        return false;
    }
    let h = host(url);
    if h.is_empty() {
        return false;
    }
    if NON_ARTICLE_HOSTS.iter().any(|b| h == *b || h.ends_with(&format!(".{b}"))) {
        return false;
    }
    let path = url.split(['?', '#']).next().unwrap_or_default().to_lowercase();
    let media_ext = [".pdf", ".jpg", ".jpeg", ".png", ".gif", ".webp", ".mp4", ".mp3", ".zip", ".doc", ".docx", ".ppt", ".pptx", ".xls"];
    if media_ext.iter().any(|e| path.ends_with(e)) {
        return false;
    }
    // Bare homepages make poor screenshots.
    url::Url::parse(url).map(|u| u.path().trim_matches('/').len() > 1).unwrap_or(false)
}

fn is_svg(c: &Candidate) -> bool {
    let url = c.media_url.as_deref().unwrap_or_default().split('?').next().unwrap_or_default().to_lowercase();
    url.ends_with(".svg") || c.extra["mime"].as_str() == Some("image/svg+xml") || c.extra["filetype"].as_str() == Some("svg")
}

fn too_small(c: &Candidate) -> bool {
    matches!((c.width, c.height), (Some(w), Some(h)) if w.min(h) < 500)
}

/// Round-robin merge so every provider's best hits surface near the top.
fn interleave(groups: Vec<Vec<Candidate>>, limit: usize) -> Vec<Candidate> {
    if limit == 0 { return Vec::new(); }
    let mut iters: Vec<_> = groups.into_iter().map(|g| g.into_iter()).collect();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut any = false;
        for it in iters.iter_mut() {
            if let Some(c) = it.next() {
                any = true;
                if !usable(&c) { continue; }
                let key = c.media_url.clone().unwrap_or_else(|| c.page_url.clone());
                if seen.insert(key) {
                    out.push(c);
                }
            }
        }
        if !any || out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

fn usable(c: &Candidate) -> bool {
    http_url(&c.page_url)
        && c.media_url.as_deref().is_none_or(http_url)
        && (matches!(c.kind, VisualKind::Article | VisualKind::Youtube) || c.media_url.is_some())
        && c.duration_s.is_none_or(|d| d.is_finite() && d > 0.0)
}

impl Sources {
    /// Exhaust free primary/alternate queries before any optional keyed
    /// service is contacted. Keep partial successes if later providers fail.
    async fn per_query<'a>(
        &'a self,
        plan: &'a VisualPlan,
        limit: usize,
        make: impl Fn(&'a Sources, String, bool) -> Vec<ProviderFut<'a>>,
        post: impl Fn(Vec<Candidate>) -> Vec<Candidate>,
    ) -> anyhow::Result<Vec<Candidate>> {
        let mut queries: Vec<String> = vec![plan.query.trim().to_string()];
        queries.extend(plan.alt_queries.iter().map(|q| q.trim().to_string()));
        let mut seen_queries = std::collections::HashSet::new();
        queries.retain(|q| !q.is_empty() && q.len() <= 2000 && seen_queries.insert(q.to_lowercase()));
        queries.truncate(3);
        let mut all: Vec<Candidate> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut any_ok = false;
        let deadline = Instant::now() + Duration::from_secs(60);
        for keyed in [false, true] {
          for q in &queries {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { break; }
            let results = futures::future::join_all(make(self, q.clone(), keyed).into_iter().map(|f| async move {
                tokio::time::timeout(remaining, f).await.unwrap_or_else(|_| ("candidate search", Err(anyhow::anyhow!("search budget exhausted"))))
            })).await;
            let mut groups = Vec::new();
            for (name, r) in results {
                match r {
                    Ok(c) => {
                        any_ok = true;
                        groups.push(c);
                    }
                    Err(e) => {
                        tracing::debug!(provider = name, query = %q, "provider failed: {e:#}");
                        errors.push(format!("{name}: {e:#}"));
                    }
                }
            }
            let merged = post(interleave(groups, usize::MAX));
            let mut seen: std::collections::HashSet<String> =
                all.iter().map(|c| c.media_url.clone().unwrap_or_else(|| c.page_url.clone())).collect();
            for c in merged {
                if seen.insert(c.media_url.clone().unwrap_or_else(|| c.page_url.clone())) {
                    all.push(c);
                }
            }
            if all.len() >= limit {
                break;
            }
          }
          if all.len() >= limit || Instant::now() >= deadline { break; }
        }
        if all.is_empty() && !any_ok && !errors.is_empty() {
            anyhow::bail!("all providers failed: {}", errors.join("; "));
        }
        all.truncate(limit);
        Ok(all)
    }

    pub(crate) async fn candidates_impl(&self, kind: VisualKind, plan: &VisualPlan, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        if limit == 0 { return Ok(Vec::new()); }
        anyhow::ensure!(plan.query.len() <= 2000, "media query exceeds 2000 bytes");
        let limit = limit.min(30);
        match kind {
            VisualKind::Text => Ok(Vec::new()),
            VisualKind::Youtube => {
                let n = (self.config.youtube.max_results as usize).max(limit).min(30);
                self.per_query(plan, limit, |s, q, keyed| if keyed { Vec::new() } else {
                    vec![boxed("youtube", async move { s.youtube_search(&q, n).await })]
                }, |c| c)
                    .await
            }
            VisualKind::Article => {
                self.per_query(
                    plan,
                    limit,
                    |s, q, keyed| {
                        if keyed { return Vec::new(); }
                        let q2 = q.clone();
                        vec![
                            boxed("news", async move { s.article_hits(&q, SearchCategory::News, limit).await }),
                            boxed("web", async move { s.article_hits(&q2, SearchCategory::General, limit).await }),
                        ]
                    },
                    |c| c,
                )
                .await
            }
            VisualKind::Meme => {
                let wanted = plan.meme.as_ref().map(|m| m.template.clone()).filter(|t| !t.trim().is_empty());
                self.per_query(plan, limit, |s, q, keyed| {
                    if keyed { return Vec::new(); }
                    let wanted = wanted.clone();
                    vec![boxed("imgflip", async move { s.meme_candidates(wanted.as_deref(), &q, limit).await })]
                }, |c| c).await
            }
            VisualKind::Gif => {
                self.per_query(
                    plan,
                    limit,
                    |s, q, keyed| {
                        if !keyed {
                            return vec![boxed("keyless gifs", async move { s.keyless_gifs(&q, limit).await })];
                        }
                        let mut v: Vec<ProviderFut> = Vec::new();
                        if s.keys.has("giphy") {
                            let q = q.clone();
                            v.push(boxed("giphy", async move { s.giphy(&q, limit).await }));
                        }
                        if s.keys.has("tenor") {
                            let q = q.clone();
                            v.push(boxed("tenor", async move { s.tenor(&q, limit).await }));
                        }
                        v
                    },
                    |c| c,
                )
                .await
            }
            VisualKind::Image => {
                self.per_query(
                    plan,
                    limit,
                    |s, q, keyed| {
                        let mut v: Vec<ProviderFut> = Vec::new();
                        if !keyed {
                        let q1 = q.clone();
                        v.push(boxed("web images", async move { s.web_images(&q1, limit).await }));
                        let q2 = q.clone();
                        v.push(boxed("wikimedia", async move { s.wikimedia_images(&q2, limit).await }));
                        let q3 = q.clone();
                        v.push(boxed("openverse", async move { s.openverse(&q3, limit).await }));
                        return v;
                        }
                        if s.keys.has("pexels") {
                            let q = q.clone();
                            v.push(boxed("pexels", async move { s.pexels_photos(&q, limit).await }));
                        }
                        if s.keys.has("unsplash") {
                            let q = q.clone();
                            v.push(boxed("unsplash", async move { s.unsplash(&q, limit).await }));
                        }
                        v
                    },
                    |c| {
                        let good: Vec<Candidate> = c.iter().filter(|c| !is_svg(c) && !too_small(c)).cloned().collect();
                        if good.is_empty() { c } else { good }
                    },
                )
                .await
            }
            VisualKind::Stock => {
                let vertical = self.config.video.format == Format::Vertical;
                let mut out = self
                    .per_query(
                        plan,
                        limit,
                        |s, q, keyed| {
                            if !keyed {
                                return vec![boxed("wikimedia", async move { s.wikimedia_videos(&q, limit).await })];
                            }
                            let mut v: Vec<ProviderFut> = Vec::new();
                            if s.keys.has("pexels") {
                                let q = q.clone();
                                v.push(boxed("pexels", async move { s.pexels_videos(&q, limit).await }));
                            }
                            if s.keys.has("pixabay") {
                                let q = q.clone();
                                v.push(boxed("pixabay", async move { s.pixabay_videos(&q, limit).await }));
                            }
                            v
                        },
                        |c| c,
                    )
                    .await?;
                // Stock is never injected into another kind's search. Even
                // here, Commons precedes optional commercial stock services.
                out.sort_by_key(|c| {
                    let portrait = matches!((c.width, c.height), (Some(w), Some(h)) if h > w);
                    let orient_ok = portrait == vertical;
                    (c.provider != "wikimedia", !orient_ok)
                });
                for c in &mut out { c.extra["explicit_stock_last_resort"] = json!(true); }
                Ok(out)
            }
        }
    }

    async fn article_hits(&self, query: &str, category: SearchCategory, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        let hits = self.web_search_impl(query, category, (limit * 3).max(10)).await?;
        Ok(hits
            .into_iter()
            .filter(|h| is_article_url(&h.url))
            .map(|h| Candidate {
                kind: VisualKind::Article,
                provider: "article".into(),
                title: h.title.clone(),
                page_url: h.url.clone(),
                media_url: None,
                thumbnail_url: h.thumbnail_url.clone().or(h.image_url.clone()),
                duration_s: None,
                width: None,
                height: None,
                start_s: None,
                extra: json!({
                    "site": host(&h.url),
                    "snippet": h.snippet,
                    "published": h.published,
                    "engine": h.engine,
                    "capture_mode": "highlighted_viewport_screenshot",
                    "scrolling_screen_recording": false,
                    "license_status": "unknown_requires_review",
                    "category": if category == SearchCategory::News { "news" } else { "general" },
                }),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_filter() {
        assert!(is_article_url("https://www.nasa.gov/missions/webb/some-story/"));
        assert!(!is_article_url("https://www.youtube.com/watch?v=abc"));
        assert!(!is_article_url("https://old.reddit.com/r/space/comments/x"));
        assert!(!is_article_url("https://example.com/paper.pdf"));
        assert!(!is_article_url("https://www.bbc.com/"));
        assert!(!is_article_url("file://example.com/story"));
    }

    #[test]
    fn merge_filters_unsafe_and_deduplicates() {
        let c = |url: &str| Candidate {
            kind: VisualKind::Image, provider: "web".into(), title: "image".into(),
            page_url: "https://example.com/source".into(), media_url: Some(url.into()),
            thumbnail_url: None, duration_s: None, width: None, height: None,
            start_s: None, extra: json!({}),
        };
        assert_eq!(interleave(vec![vec![c("file:///tmp/image")]], 10).len(), 0);
        assert_eq!(interleave(vec![vec![c("https://example.com/x")]; 2], 10).len(), 1);
        assert!(interleave(vec![vec![c("https://example.com/x")]], 0).is_empty());
    }
}
