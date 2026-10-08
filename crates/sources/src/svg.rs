//! Conservative static SVG validation. No scripts, embedded documents,
//! external resources, CSS imports, entities or unbounded dimensions.

pub(crate) fn dimensions(svg: &str) -> anyhow::Result<(u32, u32)> {
    use scraper::{Html, Selector};
    anyhow::ensure!(svg.len() <= 1024 * 1024, "SVG exceeds 1 MiB");
    let lower = svg.to_ascii_lowercase();
    anyhow::ensure!(!lower.contains("<!doctype") && !lower.contains("<!entity"), "SVG declarations are forbidden");
    let mut text = svg.trim();
    if text.starts_with("<?xml ") {
        let end = text.find("?>").ok_or_else(|| anyhow::anyhow!("invalid XML declaration"))?;
        text = text[end + 2..].trim();
    }
    anyhow::ensure!(text.starts_with("<svg") && (text.ends_with("</svg>") || text.ends_with("/>")), "not a complete SVG");
    balanced_xml(text)?;
    let doc = Html::parse_fragment(text);
    anyhow::ensure!(doc.errors.is_empty(), "malformed SVG");
    let selector = Selector::parse("svg").unwrap();
    let roots: Vec<_> = doc.select(&selector).collect();
    anyhow::ensure!(roots.len() == 1, "expected one SVG root");
    let root = roots[0];
    let allowed = ["svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon",
        "defs", "linearGradient", "radialGradient", "stop", "clipPath", "mask", "use", "symbol", "title", "desc"];
    let mut painted = false;
    let mut count = 0;
    for n in root.descendants().filter_map(scraper::ElementRef::wrap) {
        count += 1;
        anyhow::ensure!(count <= 20_000, "SVG has too many elements");
        let tag = n.value().name();
        anyhow::ensure!(allowed.contains(&tag), "unsupported/active SVG element {tag}");
        painted |= ["path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "use"].contains(&tag);
        for (name, value) in n.value().attrs() {
            let lname = name.to_ascii_lowercase();
            let v = value.trim().to_ascii_lowercase();
            anyhow::ensure!(!lname.starts_with("on"), "SVG event handlers are forbidden");
            if lname == "href" || lname.ends_with(":href") {
                anyhow::ensure!(v.starts_with('#') && !v.contains([':', '/', '&']), "external SVG reference");
            }
            if ["style", "fill", "stroke", "filter", "clip-path", "mask"].contains(&lname.as_str()) {
                anyhow::ensure!(!v.contains(['\\', '@', '&']), "unsafe SVG CSS");
                if v.contains("url") {
                    let local = regex::Regex::new(r#"url\(\s*['"]?#[a-z0-9_-]+['"]?\s*\)"#).unwrap();
                    let without = local.replace_all(&v, "");
                    anyhow::ensure!(!without.contains("url"), "external SVG CSS resource");
                }
            }
        }
    }
    anyhow::ensure!(painted, "SVG has no drawable elements");
    let number = |s: &str| s.trim().trim_end_matches("px").parse::<f64>().ok()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 8192.0);
    let view = root.value().attr("viewBox").or_else(|| root.value().attr("viewbox"))
        .and_then(|s| {
            let a: Vec<_> = s.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty())
                .map(str::parse::<f64>).collect::<Result<_, _>>().ok()?;
            (a.len() == 4 && a.iter().all(|v| v.is_finite()) && a[2] > 0.0 && a[3] > 0.0
                && a[2] <= 8192.0 && a[3] <= 8192.0).then(|| (a[2], a[3]))
        });
    let w = match root.value().attr("width") {
        Some(s) => Some(number(s).ok_or_else(|| anyhow::anyhow!("invalid SVG width"))?),
        None => view.map(|v| v.0),
    };
    let h = match root.value().attr("height") {
        Some(s) => Some(number(s).ok_or_else(|| anyhow::anyhow!("invalid SVG height"))?),
        None => view.map(|v| v.1),
    };
    let (Some(w), Some(h)) = (w, h) else { anyhow::bail!("SVG needs finite dimensions or a viewBox"); };
    Ok((w.round().max(1.0) as u32, h.round().max(1.0) as u32))
}

/// HTML parsers silently repair missing/mismatched XML end tags. Reject those
/// before the DOM-based static-resource checks; accept only quoted attributes.
fn balanced_xml(mut text: &str) -> anyhow::Result<()> {
    use std::sync::LazyLock;
    static NAME: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_.:-]*").unwrap());
    static ATTR: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"^\s+([A-Za-z_][A-Za-z0-9_.:-]*)\s*=\s*(?:"[^"<]*"|'[^'<]*')"#).unwrap());
    let mut stack = Vec::new();
    let mut roots = 0;
    while !text.is_empty() {
        let Some(start) = text.find('<') else {
            anyhow::ensure!(!stack.is_empty() || text.trim().is_empty(), "text outside SVG");
            break;
        };
        anyhow::ensure!(!stack.is_empty() || text[..start].trim().is_empty(), "text outside SVG");
        text = &text[start..];
        if text.starts_with("<!--") {
            let end = text.find("-->").ok_or_else(|| anyhow::anyhow!("unclosed SVG comment"))?;
            anyhow::ensure!(!text[4..end].contains("--"), "invalid SVG comment");
            text = &text[end+3..];
            continue;
        }
        let mut quote = None;
        let mut end = None;
        for (i,c) in text.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if q == c => quote = None,
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => { end = Some(i); break; }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| anyhow::anyhow!("unclosed SVG tag"))?;
        let tag = &text[1..end];
        text = &text[end+1..];
        if let Some(close) = tag.strip_prefix('/') {
            let close = close.trim();
            anyhow::ensure!(stack.pop() == Some(close), "mismatched SVG end tag");
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.strip_suffix('/').unwrap_or(tag);
        let name = NAME.find(tag).ok_or_else(|| anyhow::anyhow!("invalid SVG element"))?.as_str();
        if stack.is_empty() {
            roots += 1;
            anyhow::ensure!(roots == 1 && name == "svg", "expected one SVG root");
        }
        let mut attrs = &tag[name.len()..];
        let mut seen = std::collections::HashSet::new();
        while !attrs.trim().is_empty() {
            let captures = ATTR.captures(attrs).ok_or_else(|| anyhow::anyhow!("unquoted/malformed SVG attribute"))?;
            anyhow::ensure!(seen.insert(captures.get(1).unwrap().as_str()), "duplicate SVG attribute");
            attrs = &attrs[captures.get(0).unwrap().end()..];
        }
        if !self_closing { stack.push(name); }
        anyhow::ensure!(stack.len() <= 128, "SVG nesting exceeds 128");
    }
    anyhow::ensure!(roots == 1 && stack.is_empty(), "unclosed SVG elements");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_static_bounded_svg() {
        assert_eq!(dimensions(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><path d="M0 0h32v32z"/></svg>"#).unwrap(), (32, 32));
        for s in [
            "<svg><path/></svg>", "<svg viewBox=\"0 0 NaN 10\"><path/></svg>",
            "<svg width=\"32\" height=\"32\"><script>alert(1)</script></svg>",
            "<svg width=\"32\" height=\"32\"><image href=\"https://evil/x\"/></svg>",
            "<svg width=\"32\" height=\"32\" onload=\"alert(1)\"><path/></svg>",
            "<svg width=\"32\" height=\"32\"><use href=\"https://evil/x\"/></svg>",
            "<svg width=\"999999\" height=\"32\" viewBox=\"0 0 32 32\"><path/></svg>",
            "<svg width=\"32\" height=\"32\"><g><path/></svg>",
            "<svg width=\"32\" height=\"32\"><path/></g></svg>",
            "<svg width=32 height=32><path/></svg>",
        ] { assert!(dimensions(s).is_err(), "{s}"); }
    }
}
