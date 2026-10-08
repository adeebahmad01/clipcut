//! Kokoro-82M inference through ONNX Runtime. The session is created once per
//! model file and reused; inference runs on the blocking thread pool.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType};

use crate::phonemize::MAX_TOKENS;
use crate::voices::{STYLE_DIM, VoicePack};

pub const SAMPLE_RATE: u32 = 24_000;
/// Env override for ONNX Runtime intra-op threads.
pub const THREADS_ENV: &str = "CLIPCUT_TTS_THREADS";

struct Loaded {
    session: Session,
    tokens_name: String,
    style_name: String,
    speed_name: String,
    speed_type: TensorElementType,
    audio_name: String,
}

type Slot = Mutex<Option<(PathBuf, Arc<Mutex<Loaded>>)>>;

fn slot() -> &'static Slot {
    static SLOT: OnceLock<Slot> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

/// Threads for one inference: available logical CPUs, capped at 16 unless
/// overridden with [`THREADS_ENV`].
pub fn inference_threads() -> usize {
    if let Some(n) = std::env::var(THREADS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
}

fn load(model: &Path) -> anyhow::Result<Arc<Mutex<Loaded>>> {
    let mut guard = slot()
        .lock()
        .map_err(|_| anyhow::anyhow!("Kokoro session cache lock poisoned"))?;
    if let Some((path, loaded)) = guard.as_ref() {
        if path == model {
            return Ok(loaded.clone());
        }
    }
    let err =
        |e: ort::Error<ort::session::builder::SessionBuilder>| anyhow::anyhow!("onnxruntime: {e}");
    let mut builder = Session::builder()
        .map_err(|e| anyhow::anyhow!("onnxruntime: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(err)?
        .with_intra_threads(inference_threads())
        .map_err(err)?
        .with_inter_threads(1)
        .map_err(err)?;
    let session = builder
        .commit_from_file(model)
        .map_err(|e| anyhow::anyhow!("loading {}: {e}", model.display()))?;

    let mut tokens_name = None;
    let mut style_name = None;
    let mut speed = None;
    for input in session.inputs() {
        let name = input.name().to_string();
        let ty = input.dtype().tensor_type();
        match name.as_str() {
            "tokens" | "input_ids" if ty == Some(TensorElementType::Int64) => {
                tokens_name = Some(name)
            }
            "style" | "ref_s" if ty == Some(TensorElementType::Float32) => style_name = Some(name),
            "speed" => match ty {
                Some(
                    ty @ (TensorElementType::Float32
                    | TensorElementType::Int32
                    | TensorElementType::Int64),
                ) => {
                    speed = Some((name, ty));
                }
                _ => {}
            },
            _ => {}
        }
    }
    let describe = || {
        session
            .inputs()
            .iter()
            .map(|i| format!("{}: {:?}", i.name(), i.dtype()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (Some(tokens_name), Some(style_name), Some((speed_name, speed_type))) =
        (tokens_name, style_name, speed)
    else {
        anyhow::bail!("unexpected Kokoro model inputs: {}", describe());
    };
    let audio_name = session
        .outputs()
        .iter()
        .find(|o| {
            o.name() == "audio" && o.dtype().tensor_type() == Some(TensorElementType::Float32)
        })
        .or_else(|| {
            session
                .outputs()
                .first()
                .filter(|o| o.dtype().tensor_type() == Some(TensorElementType::Float32))
        })
        .map(|o| o.name().to_string())
        .ok_or_else(|| anyhow::anyhow!("Kokoro model has no float32 audio output"))?;
    tracing::info!(
        "kokoro: loaded {} ({}) with {} threads",
        model.display(),
        describe(),
        inference_threads()
    );
    let loaded = Arc::new(Mutex::new(Loaded {
        session,
        tokens_name,
        style_name,
        speed_name,
        speed_type,
        audio_name,
    }));
    *guard = Some((model.to_path_buf(), loaded.clone()));
    Ok(loaded)
}

/// Load the session ahead of time (first load takes ~1 s).
pub async fn warm(model: &Path) -> anyhow::Result<()> {
    let model = model.to_path_buf();
    tokio::task::spawn_blocking(move || load(&model).map(|_| ())).await?
}

/// Synthesize one chunk of at most [`MAX_TOKENS`] Kokoro tokens. Returns
/// mono f32 samples at [`SAMPLE_RATE`].
pub async fn infer(
    model: &Path,
    voice: &VoicePack,
    tokens: Vec<i64>,
    speed: f32,
) -> anyhow::Result<Vec<f32>> {
    anyhow::ensure!(!tokens.is_empty(), "nothing to say");
    anyhow::ensure!(
        tokens.len() <= MAX_TOKENS,
        "chunk has {} tokens (max {MAX_TOKENS})",
        tokens.len()
    );
    anyhow::ensure!(
        speed.is_finite() && speed > 0.0,
        "Kokoro speed must be finite and positive"
    );
    anyhow::ensure!(
        tokens
            .iter()
            .all(|id| crate::phonemize::VOCAB.iter().any(|(_, known)| id == known)),
        "unknown Kokoro phoneme token"
    );
    anyhow::ensure!(
        voice.0.len() >= STYLE_DIM
            && voice.0.len() % STYLE_DIM == 0
            && voice.0.iter().all(|v| v.is_finite()),
        "invalid Kokoro voice style pack"
    );
    let model = model.to_path_buf();
    let style = voice.style(tokens.len()).to_vec();
    tokio::task::spawn_blocking(move || infer_blocking(&model, tokens, style, speed)).await?
}

fn infer_blocking(
    model: &Path,
    tokens: Vec<i64>,
    style: Vec<f32>,
    speed: f32,
) -> anyhow::Result<Vec<f32>> {
    let loaded = load(model)?;
    let mut loaded = loaded
        .lock()
        .map_err(|_| anyhow::anyhow!("Kokoro inference lock poisoned"))?;
    let mut ids = Vec::with_capacity(tokens.len() + 2);
    ids.push(0);
    ids.extend(tokens);
    ids.push(0);
    let n = ids.len();
    let speed = speed.clamp(0.5, 2.0);
    let mut inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = Vec::with_capacity(3);
    inputs.push((
        Cow::Owned(loaded.tokens_name.clone()),
        Tensor::from_array(([1usize, n], ids))?.into(),
    ));
    inputs.push((
        Cow::Owned(loaded.style_name.clone()),
        Tensor::from_array(([1usize, style.len()], style))?.into(),
    ));
    let speed_name = Cow::Owned(loaded.speed_name.clone());
    match loaded.speed_type {
        TensorElementType::Int32 => {
            let rounded = speed.round().max(1.0) as i32;
            if (rounded as f32 - speed).abs() > 1e-4 {
                tracing::warn!("Kokoro export uses integer speed; rounding {speed} to {rounded}");
            }
            inputs.push((
                speed_name,
                Tensor::from_array(([1usize], vec![rounded]))?.into(),
            ));
        }
        TensorElementType::Int64 => {
            let rounded = speed.round().max(1.0) as i64;
            if (rounded as f32 - speed).abs() > 1e-4 {
                tracing::warn!("Kokoro export uses integer speed; rounding {speed} to {rounded}");
            }
            inputs.push((
                speed_name,
                Tensor::from_array(([1usize], vec![rounded]))?.into(),
            ));
        }
        _ => inputs.push((
            speed_name,
            Tensor::from_array(([1usize], vec![speed]))?.into(),
        )),
    }
    let audio_name = loaded.audio_name.clone();
    let outputs = loaded.session.run(inputs)?;
    let (_, audio) = outputs
        .get(&audio_name)
        .ok_or_else(|| anyhow::anyhow!("missing Kokoro audio output"))?
        .try_extract_tensor::<f32>()?;
    crate::audio::validate_speech(audio, SAMPLE_RATE)?;
    Ok(audio.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_inference_inputs_fail_without_loading_a_model() {
        let model = Path::new("/missing/kokoro.onnx");
        let valid_pack = VoicePack(Arc::new(vec![0.1; STYLE_DIM]));
        assert!(infer(model, &valid_pack, vec![], 1.0).await.is_err());
        assert!(
            infer(model, &valid_pack, vec![43; MAX_TOKENS + 1], 1.0)
                .await
                .is_err()
        );
        assert!(infer(model, &valid_pack, vec![43], f32::NAN).await.is_err());
        assert!(infer(model, &valid_pack, vec![-1], 1.0).await.is_err());
        let empty = VoicePack(Arc::new(vec![]));
        assert!(
            infer(model, &empty, vec![43], 1.0)
                .await
                .unwrap_err()
                .to_string()
                .contains("style pack")
        );
    }
}
