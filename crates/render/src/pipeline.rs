//! Bounded frame pipeline, parallel compositor, cancellable writer and audio
//! mix. Only a fully finished, non-cancelled render publishes its staged file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::Instant;

use anyhow::{Context, ensure};
use rayon::prelude::*;
use resvg::tiny_skia::Pixmap;

use clipcut_core::Progress;
use clipcut_core::timeline::Timeline;

use crate::audio;
use crate::control::{Control, StagedFile};
use crate::ffmpeg::{Encoder, Stream, grab_frame};
use crate::scene::Scene;
use crate::util::unpremultiply;

/// Bounded to keep 1080p RGBA batches and decoder queues affordable.
const BATCH: usize = 8;
const PREROLL_S: f32 = 0.3;

pub struct RenderStats {
    pub frames: usize,
    pub seconds: f32,
}

fn validate(tl: &Timeline, video: bool) -> anyhow::Result<()> {
    ensure!(
        tl.width >= 2 && tl.height >= 2,
        "frame dimensions must be at least 2 pixels"
    );
    ensure!(
        tl.width <= 8192 && tl.height <= 8192 && (tl.width as u64 * tl.height as u64) <= 33_554_432,
        "frame dimensions too large"
    );
    if video {
        ensure!(
            tl.width % 2 == 0 && tl.height % 2 == 0,
            "H.264/yuv420p requires even frame dimensions"
        );
    }
    ensure!((1..=120).contains(&tl.fps), "fps must be in 1..=120");
    ensure!(
        tl.duration_s.is_finite() && tl.duration_s > 0.0,
        "timeline duration must be finite and positive"
    );
    for (i, l) in tl.layers.iter().enumerate() {
        ensure!(
            l.start_s.is_finite() && l.end_s.is_finite() && l.end_s > l.start_s,
            "layer {i}: invalid time span"
        );
        ensure!(
            [l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.opacity]
                .iter()
                .all(|v| v.is_finite())
                && l.rect.w > 0.0
                && l.rect.h > 0.0
                && l.rect.w <= 4.0
                && l.rect.h <= 4.0,
            "layer {i}: invalid geometry/opacity"
        );
        ensure!(
            l.rect.w * tl.width as f32 <= 8192.0 && l.rect.h * tl.height as f32 <= 8192.0,
            "layer {i}: box too large"
        );
    }
    Ok(())
}

/// The first visible frame of a fractionally starting layer is frame zero,
/// not frame one. Use the same mapping for stills and full renders.
fn local_frame(t: f32, start: f32, fps: u32) -> usize {
    (((t - start).max(0.0) * fps as f32) + 1e-4).floor() as usize
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    tl: &Timeline,
    ffmpeg: &Path,
    sfx_dir: Option<&Path>,
    crf: u8,
    out_mp4: &Path,
    progress: &Progress,
    control: &Arc<Control>,
) -> anyhow::Result<RenderStats> {
    let t0 = Instant::now();
    control.check()?;
    validate(tl, true)?;
    let wav = StagedFile::new(out_mp4, "wav")?;
    let video = StagedFile::new(out_mp4, "mp4")?;

    progress.info("Mixing audio");
    let (mix, report) = audio::mix(tl, ffmpeg, sfx_dir, control)?;
    for w in report.warnings {
        control.check()?;
        progress.warn(w);
    }
    audio::write_wav_controlled(&wav.path, &mix, Some(control)).context("writing the audio mix")?;
    drop(mix);

    control.check()?;
    progress.info("Preparing layers");
    let scene = Scene::new(tl, None, control)?;
    let fps = tl.fps as f32;
    let total = scene.frame_count();
    let mut encoder = Encoder::start(
        ffmpeg,
        scene.w,
        scene.h,
        tl.fps,
        Some(&wav.path),
        crf,
        &video.path,
        control,
    )?;

    // A registered child makes cancellation unblock a writer's pipe write.
    let (tx, rx) = sync_channel::<Vec<Pixmap>>(1);
    let writer_control = control.clone();
    let writer = std::thread::spawn(move || -> anyhow::Result<()> {
        for batch in rx {
            for px in batch {
                writer_control.check()?;
                // H.264 is opaque; flattening is already handled by scene
                // composition over the timeline's background.
                encoder.write(px.data())?;
            }
        }
        writer_control.check()?;
        encoder.finish()
    });

    let mut streams: HashMap<usize, Stream> = HashMap::new();
    let mut frame = 0usize;
    let mut last_report = 0usize;
    let result: anyhow::Result<()> = (|| {
        while frame < total {
            control.check()?;
            let n = BATCH.min(total - frame);
            let batch_end_t = (frame + n) as f32 / fps;
            streams.retain(|_, s| s.spec.end_s > frame as f32 / fps);
            for spec in &scene.specs {
                if spec.start_s <= batch_end_t + PREROLL_S
                    && spec.end_s > frame as f32 / fps
                    && !streams.contains_key(&spec.layer)
                {
                    streams.insert(spec.layer, Stream::start(ffmpeg, spec.clone(), control)?);
                }
            }

            let mut per_frame: Vec<HashMap<usize, Arc<Pixmap>>> = Vec::with_capacity(n);
            for k in 0..n {
                control.check()?;
                let t = (frame + k) as f32 / fps;
                let mut m = HashMap::new();
                for (layer, s) in streams.iter_mut() {
                    if t >= s.spec.start_s && t < s.spec.end_s {
                        m.insert(*layer, s.frame(local_frame(t, s.spec.start_s, tl.fps))?);
                    }
                }
                per_frame.push(m);
            }

            let frames: anyhow::Result<Vec<Pixmap>> = per_frame
                .par_iter()
                .enumerate()
                .map(|(k, decoded)| {
                    control.check()?;
                    let frame = scene.draw((frame + k) as f32 / fps, decoded);
                    control.check()?;
                    Ok(frame)
                })
                .collect();
            tx.send(frames?)
                .context("encoder writer closed before all frames were rendered")?;
            frame += n;
            if frame - last_report >= 15 || frame == total {
                control.check()?;
                last_report = frame;
                progress.fraction(
                    frame as f32 / total as f32,
                    format!("Rendering frame {frame}/{total}"),
                );
            }
        }
        Ok(())
    })();
    if result.is_err() {
        // A decoding/compositor failure also promptly stops the encoder.
        control.cancel();
    }
    drop(streams);
    drop(tx);
    let finish = writer
        .join()
        .map_err(|_| anyhow::anyhow!("encoder writer thread panicked"))?;
    result?;
    finish?;
    control.publish(&video.path, out_mp4)?;
    Ok(RenderStats {
        frames: total,
        seconds: t0.elapsed().as_secs_f32(),
    })
}

pub fn still(
    tl: &Timeline,
    ffmpeg: Option<&Path>,
    at_s: f32,
    out_png: &Path,
    control: &Arc<Control>,
) -> anyhow::Result<()> {
    control.check()?;
    validate(tl, false)?;
    ensure!(at_s.is_finite(), "still timestamp must be finite");
    // Quantize to the exact displayed video frame (including the last one).
    let total = (tl.duration_s * tl.fps as f32).ceil() as usize;
    let k = ((at_s.max(0.0) * tl.fps as f32).floor() as usize).min(total - 1);
    let at = k as f32 / tl.fps as f32;
    let scene = Scene::new(tl, Some(at), control)?;
    let mut video = HashMap::new();
    ensure!(
        scene.specs.is_empty() || ffmpeg.is_some(),
        "ffmpeg is required for a still containing video"
    );
    if let Some(ff) = ffmpeg {
        let grabbed: anyhow::Result<Vec<_>> = scene
            .specs
            .par_iter()
            .map(|spec| {
                Ok((
                    spec.layer,
                    grab_frame(ff, spec, local_frame(at, spec.start_s, tl.fps), control)?,
                ))
            })
            .collect();
        video.extend(grabbed?);
    }
    control.check()?;
    let px = scene.draw(at, &video);
    let image = image::RgbaImage::from_raw(px.width(), px.height(), unpremultiply(px.take()))
        .context("frame buffer")?;
    let staged = StagedFile::new(out_png, "png")?;
    control.check()?;
    image
        .save(&staged.path)
        .with_context(|| format!("cannot write {}", out_png.display()))?;
    control.publish(&staged.path, out_png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Renderer, ffmpeg};
    use clipcut_core::timeline::{Layer, LayerContent, RectF};
    use clipcut_core::{Config, Paths, Tools};
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = PathBuf::from("/tmp/clipcut-render-smoke")
                .join(format!("test-{name}-{}-{tick}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn renderer(&self) -> Option<Renderer> {
            let config = Config::default();
            let paths = Paths {
                home: self.0.join("home"),
            };
            let tools = Tools::detect(&config, &paths);
            if tools.ffmpeg.is_none() || tools.ffprobe.is_none() {
                eprintln!("ffmpeg/ffprobe unavailable; skipping external media test");
                return None;
            }
            Some(Renderer::new(config, paths, tools))
        }

        fn clip(&self, ffmpeg: &Path) -> PathBuf {
            let path = self.0.join("clip.mp4");
            let status = Command::new(ffmpeg)
                .args([
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-threads",
                    "1",
                    "-filter_threads",
                    "1",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x48:r=4:d=1",
                    "-c:v",
                    "libx264",
                    "-crf",
                    "0",
                    "-threads",
                    "1",
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn timeline(duration: f32) -> Timeline {
        serde_json::from_value(serde_json::json!({
            "width": 64, "height": 96, "fps": 12, "duration_s": duration,
            "background": [20, 30, 50, 255], "layers": [], "captions": [],
            "caption_style": {
                "enabled": false, "font": "heavy", "size_px": 16,
                "color": [255, 255, 255, 255], "accent": [255, 0, 0, 255],
                "stroke": [0, 0, 0, 255], "y": 0.75, "max_words": 3,
                "uppercase": false
            },
            "audio": { "voice_path": null, "voice_gain": 1.0, "sfx": [], "music": null }
        }))
        .unwrap()
    }

    fn video_layer(path: &Path, duration: f32) -> Layer {
        Layer::new(
            0.01,
            duration,
            0,
            LayerContent::Video {
                path: path.to_string_lossy().into_owned(),
                trim_start_s: 0.25,
                looped: true,
            },
            RectF::FULL,
        )
    }

    #[test]
    fn fractional_layer_start_does_not_skip_its_first_frame() {
        assert_eq!(local_frame(1.0 / 30.0, 0.01, 30), 0);
        assert_eq!(local_frame(2.0 / 30.0, 0.01, 30), 1);
        assert_eq!(local_frame(3.0 / 30.0, 0.0, 30), 3);
    }

    #[test]
    fn real_decoder_loops_freezes_and_matches_still_prefix() {
        let dir = TestDir::new("decoder");
        let Some(renderer) = dir.renderer() else {
            return;
        };
        let ff = renderer.tools.ffmpeg.as_deref().unwrap();
        let path = dir.clip(ff);
        let control = Arc::new(Control::default());
        let mut spec = ffmpeg::VideoSpec {
            layer: 0,
            path: path.to_string_lossy().into_owned(),
            trim_start_s: 0.25,
            looped: true,
            width: 64,
            height: 48,
            filter: ffmpeg::video_filter(64, 48, 4, None, false, false),
            start_s: 0.0,
            end_s: 4.0,
        };
        let mut stream = Stream::start(ff, spec.clone(), &control).unwrap();
        let first = stream.frame(0).unwrap();
        let last = stream.frame(9).unwrap();
        assert_eq!(
            first.data(),
            last.data(),
            "three-frame suffix should repeat"
        );
        assert_eq!(
            last.data(),
            grab_frame(ff, &spec, 9, &control).unwrap().data()
        );
        spec.looped = false;
        let mut frozen = Stream::start(ff, spec.clone(), &control).unwrap();
        assert_eq!(
            frozen.frame(2).unwrap().data(),
            frozen.frame(12).unwrap().data()
        );
        // Trim metadata past EOF falls back once rather than hanging.
        spec.trim_start_s = 20.0;
        assert!(grab_frame(ff, &spec, 0, &control).is_ok());
        spec.trim_start_s = 0.0;
        spec.height = 64;
        spec.filter = ffmpeg::video_filter(64, 64, 4, None, true, false);
        let contained = grab_frame(ff, &spec, 0, &control).unwrap();
        assert!(contained.data()[..64 * 4].iter().all(|p| *p == 0));
        assert_eq!(contained.data()[(32 * 64 + 32) * 4 + 3], 255);
        spec.filter = ffmpeg::video_filter(
            64,
            64,
            4,
            Some(RectF::new(0.99, 0.99, 0.01, 0.01)),
            false,
            false,
        );
        assert!(
            grab_frame(ff, &spec, 0, &control).is_ok(),
            "one-pixel source crop"
        );
        // Full-HD vertical backdrops are 135x240. Subsampled source crops
        // must not silently round the odd width down and shift frame bytes.
        spec.width = 135;
        spec.height = 240;
        spec.trim_start_s = 0.75;
        spec.looped = true;
        spec.filter = ffmpeg::video_filter(135, 240, 4, None, false, true);
        let odd = grab_frame(ff, &spec, 8, &control).unwrap();
        assert_eq!((odd.width(), odd.height()), (135, 240));
    }

    #[tokio::test]
    async fn real_fractional_render_has_all_frames_and_failure_preserves_output() {
        let dir = TestDir::new("encode");
        let Some(renderer) = dir.renderer() else {
            return;
        };
        let mut tl = timeline(1.03);
        let out = dir.0.join("result.mp4");
        renderer
            .render(&tl, &out, &Progress::log_only("test"))
            .await
            .unwrap();
        let probe = Command::new(renderer.tools.ffprobe.as_ref().unwrap())
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,width,height,nb_frames",
                "-of",
                "json",
            ])
            .arg(&out)
            .output()
            .unwrap();
        assert!(probe.status.success());
        let probe: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let video = &probe["streams"][0];
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(video["width"], 64);
        assert_eq!(video["height"], 96);
        assert_eq!(video["nb_frames"], "13");
        assert_eq!(probe["streams"][1]["codec_name"], "aac");
        assert!(
            Command::new(renderer.tools.ffmpeg.as_ref().unwrap())
                .args(["-nostdin", "-v", "error", "-i",])
                .arg(&out)
                .args(["-f", "null", "-"])
                .status()
                .unwrap()
                .success()
        );
        let successful = std::fs::read(&out).unwrap();
        tl.layers
            .push(video_layer(&dir.0.join("missing.mp4"), tl.duration_s));
        let err = renderer
            .render(&tl, &out, &Progress::log_only("test"))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("decode"));
        assert_eq!(successful, std::fs::read(&out).unwrap());
        assert!(!std::fs::read_dir(&dir.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".clipcut-")
        }));
    }

    // An arbitrary user progress callback cannot be interrupted. Hold it to
    // prove that the already-cancelled blocking worker cannot later publish.
    struct ReleaseOnDrop(Arc<(Mutex<bool>, Condvar)>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let (released, wake) = &*self.0;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_kills_ffmpeg_and_old_worker_cannot_overwrite_new_render() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("cancel");
        let Some(mut renderer) = dir.renderer() else {
            return;
        };
        let clip = dir.clip(renderer.tools.ffmpeg.as_deref().unwrap());
        let log = dir.0.join("children.log");
        let wrapper = dir.0.join("ffmpeg-wrapper");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                renderer.tools.ffmpeg.as_ref().unwrap().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        renderer.tools.ffmpeg = Some(wrapper);
        let mut old = timeline(20.0);
        old.layers.push(video_layer(&clip, old.duration_s));
        let out = dir.0.join("result.mp4");
        std::fs::write(&out, b"previous output").unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let release = ReleaseOnDrop(gate.clone());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Mutex::new(Some(started_tx));
        let progress = Progress::new(
            "old",
            Arc::new(move |event| {
                if event.progress.is_some()
                    && let Some(tx) = started_tx.lock().unwrap().take()
                {
                    let _ = tx.send(());
                    let (released, wake) = &*gate;
                    let _ = wake
                        .wait_timeout_while(
                            released.lock().unwrap(),
                            Duration::from_secs(10),
                            |r| !*r,
                        )
                        .unwrap();
                }
            }),
        );
        let old_renderer = renderer.clone();
        let old_out = out.clone();
        let task =
            tokio::spawn(async move { old_renderer.render(&old, &old_out, &progress).await });
        tokio::time::timeout(Duration::from_secs(10), started_rx)
            .await
            .unwrap()
            .unwrap();
        let pids: Vec<u32> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        assert!(
            pids.len() >= 2,
            "must exercise encoder and moving-media decoder"
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(100)).await;
        for pid in &pids {
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                assert_eq!(
                    stat.split_whitespace().nth(2),
                    Some("Z"),
                    "child {pid} is still running"
                );
            }
        }
        assert_eq!(std::fs::read(&out).unwrap(), b"previous output");
        tokio::time::timeout(
            Duration::from_secs(10),
            renderer.render(&timeline(0.4), &out, &Progress::log_only("new")),
        )
        .await
        .unwrap()
        .unwrap();
        let new_video = std::fs::read(&out).unwrap();
        drop(release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_dir(&dir.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".clipcut-")
        }) {
            assert!(
                Instant::now() < deadline,
                "cancelled worker did not clean up"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            new_video,
            std::fs::read(&out).unwrap(),
            "old render overwrote the new one"
        );
        for pid in pids {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "child {pid} was not reaped"
            );
        }
    }
}
