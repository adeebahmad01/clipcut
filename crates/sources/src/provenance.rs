//! Acquisition is not rights clearance. Metadata describes the source's
//! declared license; it never asserts fair use, model releases or ownership.
use std::path::{Path, PathBuf};

use clipcut_core::{Asset, Candidate};
use serde_json::{Value, json};

/// Only unrestricted/public-domain or attribution/share-alike CC works.
pub(crate) fn permissive_license(name: &str, link: &str) -> bool {
    let name = name.trim().to_lowercase().replace(['_', ' '], "-");
    if ["-nc", "-nd", "noncommercial", "non-commercial", "no-derivatives", "no-derivs"]
        .iter().any(|s| name.contains(s)) {
        return false;
    }
    let Some((family, version)) = ["public-domain-mark", "public-domain", "cc-by-sa", "by-sa", "cc-by", "cc0", "pdm", "by"]
        .iter().find_map(|p| {
            if name == *p { return Some((*p, None)); }
            let version = name.strip_prefix(&format!("{p}-"))?;
            let numbers: Vec<_> = version.split('.').collect();
            (numbers.len() == 2 && numbers.iter().all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())))
                .then_some((*p, Some(version)))
        }) else { return false; };
    if link.is_empty() { return true; }
    let Ok(u) = url::Url::parse(link) else { return false; };
    if !crate::util::http_url(link) || !matches!(u.host_str(), Some("creativecommons.org" | "www.creativecommons.org")) {
        return false;
    }
    let p = u.path().trim_matches('/');
    // A conflicting metadata URL must not turn NC/ND into a permissive label.
    let prefix = match family {
        "cc0" => "publicdomain/zero/",
        "pdm" | "public-domain-mark" => "publicdomain/mark/",
        "public-domain" if p.starts_with("publicdomain/zero/") => "publicdomain/zero/",
        "public-domain" => "publicdomain/mark/",
        "by-sa" | "cc-by-sa" => "licenses/by-sa/",
        _ => "licenses/by/",
    };
    p.strip_prefix(prefix).is_some_and(|rest| {
        let v = rest.split('/').next().unwrap_or_default();
        !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')
            && version.is_none_or(|declared| declared == v)
    })
}

pub(crate) fn license_info(c: &Candidate) -> Value {
    let text = |k: &str| c.extra[k].as_str().unwrap_or_default().trim();
    let (name, url) = (text("license"), text("license_url"));
    let declared = !name.is_empty();
    json!({
        "status": if declared { "source_declared_license" } else { "unknown_requires_review" },
        "name": if declared { Some(name) } else { None },
        "url": if url.is_empty() { None } else { Some(url) },
        "permissive_cc_or_public_domain": permissive_license(name, url),
        "cleared": false,
        "warning": "Source metadata is not rights clearance. Verify attribution, license conditions, third-party rights and intended use before publication."
    })
}

pub(crate) fn sidecar(asset: &Path) -> PathBuf {
    asset.with_extension("source.json")
}

pub(crate) fn record(
    c: &Candidate, asset: &Asset, path: &Path, actual_media_url: Option<&str>, acquisition: Value,
) -> anyhow::Result<()> {
    let data = json!({
        "schema_version": 1, "provider": c.provider, "title": c.title,
        "page_url": c.page_url, "actual_media_url": actual_media_url,
        "license": license_info(c), "source_metadata": c.extra,
        "credit": asset.credit, "asset": asset, "acquisition": acquisition,
    });
    clipcut_core::model::write_atomic(&sidecar(path), serde_json::to_vec_pretty(&data)?.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_clears_restrictive_or_unknown_licenses() {
        for n in ["", "CC BY-NC 4.0", "CC BY-ND 2.0", "by-nc-sa", "All rights reserved", "CC BY-SA-nonsense"] {
            assert!(!permissive_license(n, ""), "{n}");
        }
        for n in ["CC BY 4.0", "CC BY-SA 3.0", "cc0", "pdm", "Public domain"] {
            assert!(permissive_license(n, ""), "{n}");
        }
        assert!(!permissive_license("CC BY 4.0", "https://creativecommons.org/licenses/by-nc/4.0/"));
        assert!(!permissive_license("CC BY 4.0", "https://evilcreativecommons.org/licenses/by/4.0/"));
        assert!(permissive_license("CC0 1.0", "https://creativecommons.org/publicdomain/zero/1.0/"));
        assert!(permissive_license("Public domain mark 1.0", "https://creativecommons.org/publicdomain/mark/1.0/"));
        assert!(!permissive_license("CC BY 4.0", "https://creativecommons.org/licenses/by/2.0/"));
    }
}
