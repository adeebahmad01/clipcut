//! Article screenshots through headless Chromium, driven over the DevTools
//! protocol (a tiny hand-rolled CDP client over a local websocket).
//!
//! One browser is shared by every capture (guarded by a mutex) and closed
//! after a short idle period or by [`shutdown_browser`].

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::Engine;
use clipcut_core::{Asset, AssetKind, Candidate, RectF, Tools};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::Sources;
use crate::util::{bounded, clear_stem, host, http_url, relative_to_project, with_ext};

/// CSS viewport; screenshots are taken at 2x => 1080x1350 px.
const VIEW_W: u32 = 540;
const VIEW_H: u32 = 675;
const SCALE: f64 = 2.0;
const IDLE_CLOSE: Duration = Duration::from_secs(60);

struct Browser {
    child: Child,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    _profile: ProfileDir,
    _group: ProcessGroup,
    document_status: HashMap<String, u16>,
    last_used: Instant,
}

struct ProfileDir(PathBuf);
impl Drop for ProfileDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
            // SAFETY: this is the browser's dedicated process group.
            if self.0 > 0 { unsafe { kill(-(self.0 as i32), 9); } }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) { let _ = self.child.start_kill(); }
}

static BROWSER: LazyLock<Mutex<Option<Browser>>> = LazyLock::new(|| Mutex::new(None));

impl Browser {
    async fn launch(chromium: &Path) -> anyhow::Result<Self> {
        let profile = std::env::temp_dir().join(format!("clipcut-chromium-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&profile)?;
        let profile_cleanup = ProfileDir(profile.clone());
        let mut command = Command::new(chromium);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.args([
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-sync",
                "--disable-background-networking",
                "--disable-dev-shm-usage",
                "--disable-features=Translate,MediaRouter,OptimizationHints,InterestFeedContentSuggestions",
                "--autoplay-policy=user-gesture-required",
                "--hide-scrollbars",
                "--mute-audio",
                "--lang=en-US",
                "--remote-debugging-port=0",
                &format!("--window-size={VIEW_W},{VIEW_H}"),
                &format!("--user-data-dir={}", profile.display()),
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start {}", chromium.display()))?;
        let group = ProcessGroup(child.id().unwrap_or(0));
        let stderr = child.stderr.take().context("no chromium stderr")?;
        let mut lines = BufReader::new(stderr).lines();
        let ws_url = tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(line) = lines.next_line().await? {
                if let Some(i) = line.find("ws://") {
                    return Ok::<_, anyhow::Error>(line[i..].trim().to_string());
                }
            }
            anyhow::bail!("chromium exited before opening DevTools")
        })
        .await
        .context("chromium did not start within 20 s")??;
        // Keep draining stderr so chromium never blocks on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&ws_url))
            .await.context("DevTools connect timed out")?.context("connecting to chromium DevTools")?;
        let mut b = Browser {
            child, ws, next_id: 0, _profile: profile_cleanup, _group: group,
            document_status: HashMap::new(), last_used: Instant::now(),
        };
        b.call(None, "Browser.getVersion", json!({}), Duration::from_secs(5)).await?;
        Ok(b)
    }

    async fn call(&mut self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let ws = &mut self.ws;
        let statuses = &mut self.document_status;
        tokio::time::timeout(timeout, async {
            ws.send(Message::Text(msg.to_string().into())).await?;
            while let Some(frame) = ws.next().await {
                let text = match frame? {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Close(_) => anyhow::bail!("chromium closed the DevTools connection"),
                    _ => continue,
                };
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v["method"].as_str() == Some("Network.responseReceived")
                    && v["params"]["type"].as_str() == Some("Document") {
                    if let (Some(frame), Some(status)) = (v["params"]["frameId"].as_str(), v["params"]["response"]["status"].as_u64()) {
                        if statuses.len() < 100 { statuses.insert(frame.to_string(), status as u16); }
                    }
                }
                if v["id"].as_u64() != Some(id) {
                    continue; // events and stale replies
                }
                if let Some(err) = v.get("error") {
                    anyhow::bail!("{method}: {}", err["message"].as_str().unwrap_or(&err.to_string()));
                }
                return Ok(v["result"].clone());
            }
            anyhow::bail!("DevTools connection ended")
        })
        .await
        .map_err(|_| anyhow::anyhow!("{method} timed out after {}s", timeout.as_secs()))?
    }

    async fn close(mut self) {
        let _ = self.call(None, "Browser.close", json!({}), Duration::from_secs(3)).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

/// Close the shared headless browser now (call on app shutdown).
pub async fn shutdown_browser() {
    if let Ok(mut guard) = tokio::time::timeout(Duration::from_secs(10), BROWSER.lock()).await {
        if let Some(b) = guard.take() { b.close().await; }
    }
}

fn schedule_idle_close() {
    tokio::spawn(async {
        tokio::time::sleep(IDLE_CLOSE + Duration::from_secs(1)).await;
        let mut guard = BROWSER.lock().await;
        if guard.as_ref().is_some_and(|b| b.last_used.elapsed() >= IDLE_CLOSE) {
            if let Some(b) = guard.take() {
                b.close().await;
            }
        }
    });
}

/// Dismiss only an explicit reject/necessary-only consent choice. Do not
/// remove access controls, paywalls, bot challenges or arbitrary overlays.
const CLEANUP_JS: &str = r#"(function(){
  const reject = /^(reject( all)?( cookies)?|decline( all)?|necessary( cookies)? only|only necessary( cookies)?|continue without accepting)$/i;
  for (const b of Array.from(document.querySelectorAll('button,[role=button]')).slice(0, 1000)) {
    const text = (b.innerText || '').trim().replace(/\s+/g, ' ');
    if (text.length < 60 && reject.test(text) && b.closest('[id*=consent i],[class*=consent i],[id*=cookie i],[class*=cookie i],[id*=cmp i],[class*=cmp i]')) {
      try { b.click(); } catch (_) {}
    }
  }
  return true;
})"#;

/// Highlight the best match of `quote` (or the headline) and scroll it to
/// ~40% of the viewport. Returns what it found.
const HIGHLIGHT_JS: &str = r#"(function(quote){
  const norm = s => s.toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[’‘`']/g, '');
  const visible = el => {
    for (let e = el; e && e !== document.body; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.display === 'none' || cs.visibility === 'hidden' || cs.opacity === '0') return false;
    }
    return true;
  };
  const collect = root => {
    const words = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {acceptNode(n) {
      const p = n.parentElement;
      if (!p || !n.nodeValue.trim() || p.closest('script,style,noscript,template,textarea,select,button,nav,footer,svg,[aria-hidden=true]')) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    }});
    const re = /[\p{L}\p{N}'’]+/gu;
    let node, lastParent = null, lastVis = true;
    let visited = 0;
    while ((node = walker.nextNode()) && ++visited <= 20000 && words.length < 50000) {
      if (node.parentElement !== lastParent) { lastParent = node.parentElement; lastVis = visible(lastParent); }
      if (!lastVis) continue;
      let m; re.lastIndex = 0;
      while ((m = re.exec(node.nodeValue))) {
        const w = norm(m[0]);
        if (w) words.push({w, node, start: m.index, end: m.index + m[0].length});
      }
    }
    return words;
  };
  const words = collect(document.body);
  const qw = (quote ? norm(quote).match(/[\p{L}\p{N}]+/gu) : null) || [];
  let range = null, how = 'none';
  if (qw.length) {
    const n = qw.length;
    // Exact word sequence first.
    outer: for (let i = 0; i + n <= words.length; i++) {
      for (let k = 0; k < n; k++) if (words[i + k].w !== qw[k]) continue outer;
      range = [i, i + n - 1]; how = 'quote'; break;
    }
    if (!range) {
      // Fuzzy: window with the most quote words, trimmed to matched ends.
      const L = Math.ceil(n * 1.3) + 1;
      const want = new Map(); qw.forEach(w => want.set(w, (want.get(w) || 0) + 1));
      let best = 0, bestRange = null;
      for (let i = 0; i < words.length; i++) {
        if (!want.has(words[i].w)) continue;
        const left = new Map(want); let score = 0, last = i;
        for (let j = i; j < Math.min(words.length, i + L); j++) {
          const c = left.get(words[j].w);
          if (c) { left.set(words[j].w, c - 1); score++; last = j; }
        }
        if (score > best) { best = score; bestRange = [i, last]; }
      }
      const need = n <= 3 ? n : Math.max(3, Math.ceil(n * 0.5));
      if (bestRange && best >= need) { range = bestRange; how = 'fuzzy'; }
    }
  }
  if (!range) {
    const h = Array.from(document.querySelectorAll('article h1, main h1, h1, [class*=headline] , [itemprop=headline]')).find(e => visible(e) && e.innerText.trim().length > 10);
    if (h) {
      const hw = collect(h);
      if (hw.length) {
        const a = words.findIndex(x => x.node === hw[0].node && x.start === hw[0].start);
        const b = words.findIndex(x => x.node === hw[hw.length - 1].node && x.start === hw[hw.length - 1].start);
        if (a >= 0 && b >= a) { range = [a, b]; how = 'headline'; }
      }
    }
  }
  const style = document.createElement('style');
  style.textContent = `mark.clipcut-hl{background:linear-gradient(180deg,rgba(255,225,40,0) 4%,rgba(255,225,40,.92) 4%,rgba(255,214,0,.92) 96%,rgba(255,225,40,0) 96%)!important;color:#111!important;padding:0 .12em!important;margin:0 -.04em!important;border-radius:.18em!important;-webkit-box-decoration-break:clone;box-decoration-break:clone;text-shadow:none!important}`;
  document.head.appendChild(style);
  let text = '';
  if (range) {
    const [a, b] = range;
    const segs = [];
    for (let i = a; i <= b; i++) {
      const w = words[i];
      const last = segs[segs.length - 1];
      if (last && last.node === w.node) last.end = w.end; else segs.push({node: w.node, start: w.start, end: w.end});
    }
    // Wrap back to front so earlier offsets stay valid.
    for (const s of segs.reverse()) {
      try {
        const r = document.createRange();
        r.setStart(s.node, s.start); r.setEnd(s.node, s.end);
        const m = document.createElement('mark'); m.className = 'clipcut-hl';
        r.surroundContents(m);
      } catch (e) {}
    }
    text = Array.from(document.querySelectorAll('mark.clipcut-hl')).map(m => m.textContent).join(' ');
  }
  window.__clipcutRect = () => {
    const rects = Array.from(document.querySelectorAll('mark.clipcut-hl')).flatMap(m => Array.from(m.getClientRects())).filter(r => r.width > 0 && r.height > 0);
    if (!rects.length) return null;
    const x0 = Math.min(...rects.map(r => r.left)), y0 = Math.min(...rects.map(r => r.top));
    const x1 = Math.max(...rects.map(r => r.right)), y1 = Math.max(...rects.map(r => r.bottom));
    return {x: x0, y: y0, w: x1 - x0, h: y1 - y0, vw: innerWidth, vh: innerHeight};
  };
  const first = document.querySelector('mark.clipcut-hl');
  if (first) {
    first.scrollIntoView({block: 'center', inline: 'nearest', behavior: 'instant'});
    const r = window.__clipcutRect();
    if (r) {
      const delta = r.h > innerHeight * 0.55 ? r.y - innerHeight * 0.12 : (r.y + r.h / 2) - innerHeight * 0.4;
      window.scrollBy({top: delta, behavior: 'instant'});
    }
  }
  const meta = s => { const e = document.querySelector(s); return e ? (e.content || '').trim() : ''; };
  return {
    how, text, url: location.href,
    site: meta('meta[property="og:site_name"]') || meta('meta[name="application-name"]') || '',
    title: (meta('meta[property="og:title"]') || document.title || '').trim(),
    textLength: (document.body ? document.body.innerText.length : 0),
  };
})"#;

fn blocked_title(title: &str) -> bool {
    let t = title.to_lowercase();
    ["just a moment", "access denied", "attention required", "are you a robot", "captcha", "403 forbidden", "404 not found", "page not found", "verify you are human", "unusual traffic", "bot verification", "security check"]
        .iter()
        .any(|p| t.contains(p))
}

struct Capture {
    png: Vec<u8>,
    focus: Option<RectF>,
    site: String,
    title: String,
    how: String,
    final_url: String,
    http_status: Option<u16>,
}

async fn eval(b: &mut Browser, session: &str, expr: String, timeout: Duration) -> anyhow::Result<Value> {
    let r = b
        .call(Some(session), "Runtime.evaluate", json!({"expression": expr, "returnByValue": true, "awaitPromise": true}), timeout)
        .await?;
    if let Some(ex) = r.get("exceptionDetails") {
        anyhow::bail!("page script failed: {}", ex["exception"]["description"].as_str().unwrap_or(&ex.to_string()));
    }
    Ok(r["result"]["value"].clone())
}

async fn capture_in(b: &mut Browser, url: &str, quote: Option<&str>) -> anyhow::Result<Capture> {
    let target = b.call(None, "Target.createTarget", json!({"url": "about:blank"}), Duration::from_secs(10)).await?;
    let target_id = target["targetId"].as_str().context("no targetId")?.to_string();
    let result = async {
        let attached = b
            .call(None, "Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), Duration::from_secs(10))
            .await?;
        let session = attached["sessionId"].as_str().context("no sessionId")?.to_string();
        let s = Some(session.as_str());
        let t = Duration::from_secs(10);
        b.call(s, "Page.enable", json!({}), t).await?;
        b.document_status.clear();
        b.call(s, "Network.enable", json!({}), t).await?;
        b.call(
            s,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": VIEW_W, "height": VIEW_H, "deviceScaleFactor": SCALE, "mobile": false}),
            t,
        )
        .await?;
        let nav = b.call(s, "Page.navigate", json!({"url": url}), Duration::from_secs(25)).await?;
        if let Some(err) = nav["errorText"].as_str().filter(|e| !e.is_empty()) {
            anyhow::bail!("could not open {url}: {err}");
        }
        // Wait for load (bounded), then a beat for late layout.
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let state = eval(b, &session, "document.readyState".into(), Duration::from_secs(5)).await.unwrap_or_default();
            if state.as_str() == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let _ = eval(b, &session, format!("{CLEANUP_JS}()"), Duration::from_secs(5)).await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        let q = serde_json::to_string(&quote.unwrap_or_default())?;
        let info = eval(b, &session, format!("{HIGHLIGHT_JS}({q})"), Duration::from_secs(15)).await?;
        let title = info["title"].as_str().unwrap_or_default().to_string();
        let text_len = info["textLength"].as_u64().unwrap_or(0);
        let final_url = info["url"].as_str().context("page has no final URL")?.to_string();
        anyhow::ensure!(http_url(&final_url), "page redirected to a non-HTTP(S) URL");
        let http_status = nav["frameId"].as_str().and_then(|frame| b.document_status.get(frame)).copied();
        anyhow::ensure!(http_status.is_none_or(|s| s < 400), "article returned HTTP {}", http_status.unwrap_or_default());
        if blocked_title(&title) || text_len < 200 {
            anyhow::bail!("{url} looks blocked or empty (title {title:?}, {text_len} chars)");
        }
        // Lazy content and scroll-triggered headers settle, then clean again.
        tokio::time::sleep(Duration::from_millis(700)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let rect = eval(b, &session, "window.__clipcutRect ? window.__clipcutRect() : null".into(), Duration::from_secs(5)).await?;
        let focus = rect.as_object().and_then(|_| {
            let (vw, vh) = (rect["vw"].as_f64()?, rect["vh"].as_f64()?);
            let x0 = (rect["x"].as_f64()? / vw).clamp(0.0, 1.0);
            let y0 = (rect["y"].as_f64()? / vh).clamp(0.0, 1.0);
            let x1 = ((rect["x"].as_f64()? + rect["w"].as_f64()?) / vw).clamp(0.0, 1.0);
            let y1 = ((rect["y"].as_f64()? + rect["h"].as_f64()?) / vh).clamp(0.0, 1.0);
            (x1 > x0 && y1 > y0).then(|| RectF::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32))
        });
        let shot = b
            .call(s, "Page.captureScreenshot", json!({"format": "png", "captureBeyondViewport": false}), Duration::from_secs(20))
            .await?;
        let data = shot["data"].as_str().context("no screenshot data")?;
        anyhow::ensure!(data.len() <= 16 * 1024 * 1024, "screenshot exceeds decode budget");
        let png = base64::engine::general_purpose::STANDARD.decode(data)?;
        Ok(Capture {
            png,
            focus,
            site: info["site"].as_str().unwrap_or_default().to_string(),
            title,
            how: info["how"].as_str().unwrap_or("none").to_string(),
            final_url, http_status,
        })
    }
    .await;
    let _ = b.call(None, "Target.closeTarget", json!({"targetId": target_id}), Duration::from_secs(5)).await;
    result
}

impl Sources {
    pub(crate) async fn fetch_article(&self, c: &Candidate, dest_dir: &Path, stem: &str, quote: Option<&str>) -> anyhow::Result<Asset> {
        let chromium = Tools::require(&self.tools.chromium, "chromium")?.to_path_buf();
        anyhow::ensure!(quote.is_none_or(|q| q.len() <= 2000), "highlight quote exceeds 2000 bytes");
        let cap = {
            let mut guard = tokio::time::timeout(Duration::from_secs(10), BROWSER.lock())
                .await.context("article browser queue exceeded 10 s")?;
            // The active capture owns the browser. If the caller cancels,
            // Drop kills it and removes its isolated temporary profile.
            let mut b = match guard.take() {
                Some(b) => b,
                None => bounded("browser launch", Duration::from_secs(30), Browser::launch(&chromium)).await?,
            };
            let r = bounded("article capture", Duration::from_secs(75), capture_in(&mut b, &c.page_url, quote)).await;
            match r {
                Ok(cap) => {
                    b.last_used = Instant::now();
                    *guard = Some(b);
                    cap
                }
                Err(e) => { b.close().await; return Err(e); }
            }
        };
        schedule_idle_close();
        let decoded = crate::media::decode_image(&cap.png)?;
        let (w, h) = (decoded.width(), decoded.height());
        anyhow::ensure!((w, h) == (1080, 1350), "unexpected article screenshot size {w}x{h}");
        tokio::fs::create_dir_all(dest_dir).await?;
        clear_stem(dest_dir, stem);
        let out = with_ext(dest_dir, stem, "png");
        tokio::fs::write(&out, &cap.png).await?;
        let site = if cap.site.is_empty() { host(&cap.final_url) } else { cap.site.clone() };
        let title = if cap.title.is_empty() { c.title.clone() } else { cap.title.clone() };
        tracing::debug!(url = %c.page_url, how = %cap.how, "article captured");
        let asset = Asset {
            kind: AssetKind::Image,
            path: relative_to_project(dest_dir, &out),
            provider: "article".into(),
            source_url: cap.final_url.clone(),
            credit: format!("{site}: \"{title}\" ({}) — viewport screenshot; license unknown; rights review required (not cleared)", cap.final_url),
            width: w,
            height: h,
            duration_s: None,
            trim_start_s: 0.0,
            focus: cap.focus,
        };
        crate::provenance::record(c, &asset, &out, None, json!({
            "mode": "highlighted_viewport_screenshot", "scrolling_screen_recording": false,
            "final_url": cap.final_url, "http_status": cap.http_status,
            "highlight_mode": cap.how, "requested_quote": quote,
            "access_controls_bypassed": false
        }))?;
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_does_not_bypass_access_controls() {
        assert!(!CLEANUP_JS.contains(".remove("));
        assert!(!CLEANUP_JS.contains("acceptRe"));
        assert!(blocked_title("403 Forbidden"));
        assert!(blocked_title("Verify you are human"));
        assert!(!blocked_title("Coffee cherries are drupes"));
    }
}
