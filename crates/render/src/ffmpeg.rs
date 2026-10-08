//! ffmpeg plumbing. Every render child is registered with its cancellation
//! control; owners kill/reap it on all exit paths. Decoder queues are bounded.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, bail};
use resvg::tiny_skia::Pixmap;

use clipcut_core::timeline::RectF;

use crate::control::{Control, ManagedChild};
use crate::util::{bounded_crop, pixmap_from_rgba};

/// What a decoder must produce for one layer.
#[derive(Debug, Clone)]
pub struct VideoSpec {
    pub layer: usize,
    pub path: String,
    pub trim_start_s: f32,
    pub looped: bool,
    pub width: u32,
    pub height: u32,
    /// `-vf` chain producing exactly `width`x`height`.
    pub filter: String,
    pub start_s: f32,
    pub end_s: f32,
}

/// Optional source crop, then cover-crop (or transparent contain-pad), blur
/// and frame-rate conversion. Source crops cannot extend outside the source.
pub fn video_filter(
    w: u32,
    h: u32,
    fps: u32,
    crop: Option<RectF>,
    contain: bool,
    blur: bool,
) -> String {
    let mut f = Vec::new();
    if let Some(c) = crop {
        let c = bounded_crop(c);
        f.push(format!(
            "crop=max(1\\,floor(iw*{:.6})):max(1\\,floor(ih*{:.6})):floor(iw*{:.6}):floor(ih*{:.6}):exact=1",
            c.w, c.h, c.x, c.y
        ));
    }
    let flags = if blur { "fast_bilinear" } else { "bicubic" };
    if contain {
        f.push(format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:flags={flags}"
        ));
        f.push("format=rgba".into());
        f.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0"));
    } else {
        f.push(format!(
            "scale={w}:{h}:force_original_aspect_ratio=increase:flags={flags}"
        ));
        // Layers/backdrops need not have even dimensions. The default crop
        // rounds down for subsampled input, corrupting raw RGBA boundaries.
        f.push(format!("crop={w}:{h}:exact=1"));
    }
    if blur {
        f.push("gblur=sigma=2.2".into());
        f.push("eq=brightness=-0.06:saturation=1.3".into());
        f.push("colorchannelmixer=rr=0.55:gg=0.55:bb=0.55".into());
    }
    f.push("setsar=1".into());
    f.push(format!("fps={fps}"));
    f.join(",")
}

fn decoder_command(ffmpeg: &Path, spec: &VideoSpec, trim: f32) -> Command {
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "1",
        "-filter_threads",
        "1",
    ]);
    if spec.path.to_ascii_lowercase().ends_with(".gif") {
        cmd.args(["-ignore_loop", "1"]);
    }
    if trim > 0.0 {
        cmd.args(["-ss", &format!("{trim:.6}")]);
    }
    cmd.args(["-i", &spec.path, "-an", "-sn", "-vf", &spec.filter]);
    cmd.args(["-threads", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Always drain the pipe, but retain only a bounded error tail.
fn drain_stderr(mut err: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = err.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 65_536 {
                tail.drain(..tail.len() - 65_536);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

/// Frames arrive on a helper thread, at most four ahead of the compositor.
/// EOF loops from the trim point, or freezes the final frame when non-looped.
pub struct Stream {
    pub spec: VideoSpec,
    ffmpeg: PathBuf,
    control: Arc<Control>,
    child: Option<ManagedChild>,
    reader: Option<JoinHandle<anyhow::Result<()>>>,
    stderr: Option<JoinHandle<String>>,
    rx: Option<Receiver<Pixmap>>,
    next: usize,
    last: Option<Arc<Pixmap>>,
    eof: bool,
    cycle_frames: usize,
    trim: f32,
    tried_start: bool,
}

impl Stream {
    pub fn start(ffmpeg: &Path, spec: VideoSpec, control: &Arc<Control>) -> anyhow::Result<Self> {
        let trim = spec.trim_start_s.max(0.0);
        let mut s = Stream {
            spec,
            ffmpeg: ffmpeg.to_path_buf(),
            control: control.clone(),
            child: None,
            reader: None,
            stderr: None,
            rx: None,
            next: 0,
            last: None,
            eof: false,
            cycle_frames: 0,
            trim,
            tried_start: false,
        };
        s.spawn()?;
        Ok(s)
    }

    fn spawn(&mut self) -> anyhow::Result<()> {
        self.stop();
        self.control.check()?;
        let frame_bytes = self.spec.width as usize * self.spec.height as usize * 4;
        let (w, h) = (self.spec.width, self.spec.height);
        let child = ManagedChild::spawn(
            &mut decoder_command(&self.ffmpeg, &self.spec, self.trim),
            &self.control,
        )
        .with_context(|| format!("starting video decoder for {}", self.spec.path))?;
        let mut stdout = child.with(|c| c.stdout.take()).context("decoder stdout")?;
        let err = child.with(|c| c.stderr.take()).context("decoder stderr")?;
        self.stderr = Some(drain_stderr(err));
        let (tx, rx) = sync_channel(4);
        let control = self.control.clone();
        self.reader = Some(std::thread::spawn(move || {
            loop {
                control.check()?;
                let mut buf = vec![0u8; frame_bytes];
                let mut n = 0;
                while n < frame_bytes {
                    let got = stdout.read(&mut buf[n..])?;
                    if got == 0 {
                        if n == 0 {
                            return Ok(());
                        }
                        bail!("truncated raw video frame ({n}/{frame_bytes} bytes)");
                    }
                    n += got;
                }
                control.check()?;
                let px =
                    pixmap_from_rgba(w, h, buf).context("invalid raw video frame dimensions")?;
                if tx.send(px).is_err() {
                    return Ok(());
                }
            }
        }));
        self.child = Some(child);
        self.rx = Some(rx);
        self.cycle_frames = 0;
        Ok(())
    }

    fn stop(&mut self) {
        // Release a reader blocked on its bounded queue before joining it.
        self.rx = None;
        if let Some(c) = &self.child {
            c.kill();
        }
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        if let Some(h) = self.stderr.take() {
            let _ = h.join();
        }
        self.child = None;
    }

    fn finish_cycle(&mut self) -> anyhow::Result<()> {
        let read = self
            .reader
            .take()
            .context("missing decoder reader")?
            .join()
            .map_err(|_| anyhow::anyhow!("video reader panicked"))?;
        let status = self
            .child
            .as_ref()
            .context("missing decoder child")?
            .wait()?;
        let err = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        self.control.check()?;
        read.with_context(|| format!("reading {}", self.spec.path))?;
        if !status.success() {
            bail!(
                "ffmpeg could not decode {} ({status}): {}",
                self.spec.path,
                err.trim()
            );
        }
        Ok(())
    }

    /// Layer-local output frame `k`; calls must be monotonic.
    pub fn frame(&mut self, k: usize) -> anyhow::Result<Arc<Pixmap>> {
        while self.next <= k && !self.eof {
            self.control.check()?;
            let got = self
                .rx
                .as_ref()
                .context("decoder queue closed")?
                .recv_timeout(Duration::from_millis(50));
            match got {
                Ok(px) => {
                    self.last = Some(Arc::new(px));
                    self.cycle_frames += 1;
                    self.next += 1;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.finish_cycle()?;
                    if self.cycle_frames == 0 && self.trim > 0.0 && !self.tried_start {
                        // Bad trim metadata should not turn valid media into a blank card.
                        self.trim = 0.0;
                        self.tried_start = true;
                        self.spawn()?;
                    } else if self.spec.looped && self.cycle_frames > 0 {
                        self.spawn()?;
                    } else {
                        self.eof = true;
                        self.stop();
                    }
                }
            }
        }
        self.control.check()?;
        self.last
            .clone()
            .with_context(|| format!("no video frames in {}", self.spec.path))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Decode the same prefix as the render pipeline. This deliberately shares
/// trim/loop/final-frame behavior rather than seeking past EOF for previews.
pub fn grab_frame(
    ffmpeg: &Path,
    spec: &VideoSpec,
    local_frame: usize,
    control: &Arc<Control>,
) -> anyhow::Result<Arc<Pixmap>> {
    Stream::start(ffmpeg, spec.clone(), control)?.frame(local_frame)
}

/// x264 encoder. Drop closes the pipe, terminates and reaps unfinished work.
pub struct Encoder {
    child: ManagedChild,
    control: Arc<Control>,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<String>>,
}

impl Encoder {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        ffmpeg: &Path,
        w: u32,
        h: u32,
        fps: u32,
        audio: Option<&Path>,
        crf: u8,
        out: &Path,
        control: &Arc<Control>,
    ) -> anyhow::Result<Self> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-filter_threads",
            "1",
        ]);
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{w}x{h}"),
            "-r",
            &fps.to_string(),
            "-i",
            "-",
        ]);
        if let Some(a) = audio {
            cmd.arg("-i").arg(a);
        }
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            &crf.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-threads",
            "2",
        ]);
        if audio.is_some() {
            // The WAV is padded to the exact number of video frames. Do not
            // use -shortest: AAC packet rounding can otherwise drop a frame.
            cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        }
        cmd.args(["-movflags", "+faststart"]).arg(out);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child =
            ManagedChild::spawn(&mut cmd, control).context("failed to start ffmpeg encoder")?;
        let stdin = child.with(|c| c.stdin.take());
        let err = child.with(|c| c.stderr.take()).context("encoder stderr")?;
        Ok(Self {
            child,
            control: control.clone(),
            stdin,
            stderr: Some(drain_stderr(err)),
        })
    }

    pub fn write(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        self.control.check()?;
        if let Err(e) = self
            .stdin
            .as_mut()
            .context("encoder closed")?
            .write_all(frame)
        {
            self.stdin = None;
            self.child.kill();
            let _ = self.child.wait();
            let msg = self
                .stderr
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_default();
            self.control.check()?;
            bail!("ffmpeg encoder stopped ({e}): {}", msg.trim());
        }
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<()> {
        self.stdin = None;
        let status = self.child.wait()?;
        let err = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        self.control.check()?;
        if !status.success() {
            bail!("ffmpeg encoder exited with {status}: {}", err.trim());
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stdin = None;
        self.child.kill();
        if let Some(h) = self.stderr.take() {
            let _ = h.join();
        }
    }
}

/// Decode any audio to interleaved f32 at 48 kHz. Killing its registered
/// child interrupts the pipe read when the render future is dropped.
#[allow(clippy::too_many_arguments)]
pub fn decode_audio(
    ffmpeg: &Path,
    path: &Path,
    channels: u32,
    start_s: f32,
    duration_s: Option<f32>,
    loop_forever: bool,
    control: &Control,
) -> anyhow::Result<Vec<f32>> {
    if loop_forever && duration_s.is_none() {
        bail!("looped audio needs a finite duration");
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "1",
        "-filter_threads",
        "1",
    ]);
    if loop_forever {
        cmd.args(["-stream_loop", "-1"]);
    }
    if start_s > 0.0 {
        cmd.args(["-ss", &format!("{start_s:.6}")]);
    }
    cmd.arg("-i").arg(path);
    if let Some(d) = duration_s {
        cmd.args(["-t", &format!("{:.6}", d.max(0.000_001))]);
    }
    cmd.args([
        "-vn",
        "-ac",
        &channels.to_string(),
        "-ar",
        "48000",
        "-f",
        "f32le",
        "-",
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = ManagedChild::spawn(&mut cmd, control)
        .with_context(|| format!("failed to run ffmpeg on {}", path.display()))?;
    let mut stdout = child.with(|c| c.stdout.take()).context("audio stdout")?;
    let err = drain_stderr(child.with(|c| c.stderr.take()).context("audio stderr")?);
    let mut bytes = Vec::new();
    let read = stdout.read_to_end(&mut bytes);
    if read.is_err() {
        child.kill();
    }
    let status = child.wait()?;
    let err = err.join().unwrap_or_default();
    control.check()?;
    read?;
    if !status.success() {
        bail!("ffmpeg could not decode {}: {}", path.display(), err.trim());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Width, height and duration via ffprobe (sync, used by the director).
pub fn probe(ffprobe: &Path, path: &Path) -> Option<(u32, u32, f32)> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let s = &v["streams"][0];
    let w = s["width"].as_u64()? as u32;
    let h = s["height"].as_u64()? as u32;
    let d = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    Some((w, h, d))
}
