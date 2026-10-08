//! Iconify icons: direct ids ("mdi:fire") or a search word ("fire"),
//! preferring colorful emoji sets, cached as SVG files.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Sources;
use crate::util::{REQUEST_TIMEOUT, bounded, endpoint, json_response, read_fresh, slug, text_response};

const API: &str = "https://api.iconify.design";

/// Preferred icon sets, best first. Colorful emoji sets read well on video.
const PREFERRED: &[&str] = &["fluent-emoji-flat", "noto", "twemoji", "openmoji", "fluent-emoji", "emojione", "mdi", "ph", "tabler", "lucide"];

fn valid_id(id: &str) -> bool {
    if id.len() > 160 { return false; }
    let mut parts = id.splitn(2, ':');
    let (Some(set), Some(name)) = (parts.next(), parts.next()) else { return false };
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    ok(set) && ok(name)
}

/// Pick the best icon id from an Iconify search response.
pub(crate) fn pick_icon(ids: &[String], query: &str) -> Option<String> {
    let q = slug(query);
    let score = |id: &String| {
        let (set, name) = id.split_once(':').unwrap_or(("", id));
        let set_rank = PREFERRED.iter().position(|p| *p == set).unwrap_or(PREFERRED.len());
        // Exact name match beats a partial one within the same tier of sets.
        let name_rank = if name == q {
            0
        } else if name.starts_with(&format!("{q}-")) || name.ends_with(&format!("-{q}")) {
            1
        } else {
            2
        };
        // Emoji sets dominate; inside them prefer the exact name.
        (set_rank.min(4) / 4, name_rank, set_rank, name.len())
    };
    ids.iter().filter(|id| valid_id(id)).min_by_key(|id| score(id)).cloned()
}

/// Ensure the root `<svg>` has explicit width/height so rasterizers size it.
pub(crate) fn ensure_svg_size(svg: &str, size: u32) -> String {
    let Some(start) = svg.find("<svg") else { return svg.to_string() };
    let Some(rel_end) = svg[start..].find('>') else { return svg.to_string() };
    let end = start + rel_end;
    let tag = &svg[start + 4..end];
    let attr = regex::Regex::new(r#"\s+(?:width|height)\s*=\s*(?:"[^"]*"|'[^']*')"#).unwrap();
    let tag = attr.replace_all(tag, "");
    format!("{} width=\"{size}\" height=\"{size}\"{}{}", &svg[..start + 4], tag, &svg[end..])
}

impl Sources {
    pub(crate) async fn icon_svg_impl(&self, name_or_query: &str) -> anyhow::Result<Option<PathBuf>> {
        bounded("icon lookup", Duration::from_secs(30), self.icon_svg_bounded(name_or_query)).await
    }

    async fn icon_svg_bounded(&self, name_or_query: &str) -> anyhow::Result<Option<PathBuf>> {
        anyhow::ensure!(name_or_query.len() <= 500, "icon query exceeds 500 bytes");
        let input = name_or_query.trim().to_lowercase();
        if input.is_empty() {
            return Ok(None);
        }
        let dir = self.paths.cache().join("icons");
        tokio::fs::create_dir_all(&dir).await?;
        let id = if input.contains(':') {
            anyhow::ensure!(valid_id(&input), "invalid Iconify id {input:?}");
            input.clone()
        } else {
            // Positive and negative query mappings expire; damaged mappings
            // must not become path components or panic at split_once.
            let map = dir.join(format!("q-{}.txt", slug(&input)));
            let cached = read_fresh(&map, Duration::from_secs(24 * 3600));
            if cached.as_deref().is_some_and(|s| s.trim().is_empty()) {
                return Ok(None);
            }
            if let Some(cached) = cached.filter(|s| valid_id(s.trim())) {
                cached.trim().to_string()
            } else {
                match self.iconify_search(&input).await? {
                    Some(id) => {
                        clipcut_core::model::write_atomic(&map, id.as_bytes())?;
                        id
                    }
                    None => {
                        clipcut_core::model::write_atomic(&map, b"")?;
                        return Ok(None);
                    }
                }
            }
        };
        let file = dir.join(format!("{}.svg", id.replace(':', "__")));
        let sidecar = crate::provenance::sidecar(&file);
        if let Some(cached) = read_fresh(&file, Duration::from_secs(30 * 24 * 3600)) {
            if crate::svg::dimensions(&cached).is_ok() && sidecar.is_file() {
                return Ok(Some(file));
            }
        }
        let (set, name) = id.split_once(':').ok_or_else(|| anyhow::anyhow!("invalid cached Iconify id"))?;
        let url = format!("{API}/{set}/{name}.svg?height=256");
        let resp = self.http.get(endpoint("iconify", &url)).query(&[("height", "256")]).timeout(REQUEST_TIMEOUT).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = text_response("iconify", resp).await?;
        if !body.trim_start().starts_with("<svg") {
            // Iconify answers unknown icons with "404" as text.
            return Ok(None);
        }
        let svg = ensure_svg_size(&body, 256);
        let (width, height) = crate::svg::dimensions(&svg)?;
        let info = self.iconify_collection(set).await.unwrap_or(Value::Null);
        let license = &info["license"];
        let name = license["title"].as_str().or(license["spdx"].as_str()).or(license.as_str());
        let license_url = license["url"].as_str().filter(|u| crate::util::http_url(u));
        let metadata = json!({
            "schema_version": 1, "provider": "iconify", "id": id, "page_url": format!("https://icon-sets.iconify.design/{set}/{name_id}/", name_id=id.split_once(':').unwrap().1),
            "actual_media_url": url, "width": width, "height": height,
            "source_metadata": info,
            "license": {
                "status": if name.is_some() { "source_declared_license" } else { "unknown_requires_review" },
                "name": name, "url": license_url, "cleared": false,
                "warning": "Iconify aggregates independently licensed sets. Verify the collection license, attribution and third-party rights before publication."
            },
            "acquisition": {"mode": "validated_static_svg", "requested_size": 256}
        });
        clipcut_core::model::write_atomic(&sidecar, &serde_json::to_vec_pretty(&metadata)?)?;
        clipcut_core::model::write_atomic(&file, svg.as_bytes())?;
        Ok(Some(file))
    }

    async fn iconify_search(&self, query: &str) -> anyhow::Result<Option<String>> {
        let words = crate::util::content_words(query);
        let mut queries = vec![query];
        if words.len() > 1 {
            queries.push(words.last().unwrap());
        }
        for q in queries {
            let resp = self.http.get(endpoint("iconify", &format!("{API}/search")))
                .query(&[("query", q), ("limit", "256")]).timeout(REQUEST_TIMEOUT).send().await?;
            let v = json_response("iconify", resp).await?;
            let ids: Vec<String> = v["icons"].as_array().into_iter().flatten()
                .filter_map(|i| i.as_str().filter(|id| valid_id(id)).map(str::to_string)).take(256).collect();
            if let Some(id) = pick_icon(&ids, query) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    async fn iconify_collection(&self, prefix: &str) -> anyhow::Result<Value> {
        let resp = self.http.get(endpoint("iconify", &format!("{API}/collection")))
            .query(&[("prefix", prefix), ("info", "true")]).timeout(REQUEST_TIMEOUT).send().await?;
        let v = json_response("iconify", resp).await?;
        Ok(v["info"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_emoji_sets_and_exact_names() {
        let ids: Vec<String> = ["mdi:fire", "tabler:flame", "noto:fire", "fluent-emoji-flat:fire", "fluent-emoji-flat:fire-engine"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_icon(&ids, "fire").as_deref(), Some("fluent-emoji-flat:fire"));
        let ids: Vec<String> = ["mdi:telescope", "ph:telescope-bold"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_icon(&ids, "telescope").as_deref(), Some("mdi:telescope"));
    }

    #[test]
    fn adds_svg_size() {
        let s = ensure_svg_size(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><path/></svg>"#, 256);
        assert!(s.starts_with(r#"<svg width="256" height="256" xmlns"#));
        let s2 = ensure_svg_size(r#"<svg width="1em" height="1em" viewBox="0 0 32 32"></svg>"#, 256);
        assert!(s2.contains(r#"width="256""#));
        assert!(!s2.contains("1em"));
    }

    #[test]
    fn rejects_invalid_icon_ids() {
        for bad in ["", "mdi", "mdi:../fire", ":fire", "mdi:fire:extra", "https://evil/svg"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(pick_icon(&["../bad".into()], "fire").is_none());
    }
}
