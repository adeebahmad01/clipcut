//! Kokoro voice packs (`voices-v1.0.bin`, an npz of float32 (510, 1, 256)
//! arrays), the English voice catalog and voice blending.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const STYLE_DIM: usize = 256;
pub const PACK_ROWS: usize = 510;

/// English Kokoro v1.0 voices: (id, display name).
pub const KOKORO_VOICES: &[(&str, &str)] = &[
    ("af_heart", "Heart"),
    ("af_bella", "Bella"),
    ("af_nicole", "Nicole"),
    ("af_aoede", "Aoede"),
    ("af_kore", "Kore"),
    ("af_sarah", "Sarah"),
    ("af_nova", "Nova"),
    ("af_sky", "Sky"),
    ("af_alloy", "Alloy"),
    ("af_jessica", "Jessica"),
    ("af_river", "River"),
    ("am_michael", "Michael"),
    ("am_fenrir", "Fenrir"),
    ("am_puck", "Puck"),
    ("am_echo", "Echo"),
    ("am_eric", "Eric"),
    ("am_liam", "Liam"),
    ("am_onyx", "Onyx"),
    ("am_santa", "Santa"),
    ("am_adam", "Adam"),
    ("bf_emma", "Emma"),
    ("bf_isabella", "Isabella"),
    ("bf_alice", "Alice"),
    ("bf_lily", "Lily"),
    ("bm_george", "George"),
    ("bm_fable", "Fable"),
    ("bm_lewis", "Lewis"),
    ("bm_daniel", "Daniel"),
];

/// espeak-ng fallback voices: (id, label).
pub const ESPEAK_VOICES: &[(&str, &str)] = &[
    ("en-us+m3", "eSpeak US male"),
    ("en-us+f3", "eSpeak US female"),
    ("en-us+m7", "eSpeak US male (deep)"),
    ("en-gb+m3", "eSpeak UK male"),
    ("en-gb+f3", "eSpeak UK female"),
];

/// "Michael (US male)" for `am_michael`.
pub fn kokoro_label(id: &str) -> String {
    let name = KOKORO_VOICES
        .iter()
        .find(|(v, _)| *v == id)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| {
            let raw = id.split_once('_').map(|(_, n)| n).unwrap_or(id);
            let mut c = raw.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        });
    let accent = match id.chars().next() {
        Some('a') => "US",
        Some('b') => "UK",
        _ => "",
    };
    let gender = match id.chars().nth(1) {
        Some('f') => "female",
        Some('m') => "male",
        _ => "",
    };
    format!(
        "{name} ({})",
        [accent, gender]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

pub fn is_kokoro_voice(id: &str) -> bool {
    KOKORO_VOICES.iter().any(|(v, _)| *v == id)
}

/// Parse `af_heart`, `af_heart+am_michael` or `af_heart:0.6+am_michael:0.4`.
/// Weights are normalized to sum to 1.
pub fn parse_blend(spec: &str) -> anyhow::Result<Vec<(String, f32)>> {
    let mut parts = Vec::new();
    for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, weight) = match part.split_once([':', '*']) {
            Some((n, w)) => {
                let w: f32 = w
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad voice weight in {part:?}"))?;
                (n.trim(), w)
            }
            None => (part, 1.0),
        };
        anyhow::ensure!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "bad voice name {name:?}"
        );
        anyhow::ensure!(
            weight > 0.0 && weight.is_finite(),
            "voice weight must be positive in {part:?}"
        );
        parts.push((name.to_string(), weight));
    }
    anyhow::ensure!(!parts.is_empty(), "empty voice");
    let total: f64 = parts.iter().map(|(_, w)| *w as f64).sum();
    for (_, w) in &mut parts {
        *w = (*w as f64 / total) as f32;
        anyhow::ensure!(
            *w > 0.0,
            "voice weights differ too much to represent their blend"
        );
    }
    Ok(parts)
}

/// Whether `spec` names Kokoro voices only (single or blended).
pub fn is_kokoro_spec(spec: &str) -> bool {
    parse_blend(spec)
        .map(|parts| parts.iter().all(|(n, _)| is_kokoro_voice(n)))
        .unwrap_or(false)
}

/// A loaded voice pack: PACK_ROWS style vectors of STYLE_DIM floats.
#[derive(Debug, Clone)]
pub struct VoicePack(pub Arc<Vec<f32>>);

impl VoicePack {
    /// Style vector for a phoneme sequence of `tokens` tokens (row `n - 1`,
    /// as in hexgrad's KPipeline and kokoro-onnx).
    pub fn style(&self, tokens: usize) -> &[f32] {
        let rows = self.0.len() / STYLE_DIM;
        if rows == 0 {
            return &[];
        }
        let row = tokens.clamp(1, rows) - 1;
        &self.0[row * STYLE_DIM..(row + 1) * STYLE_DIM]
    }
}

type Cache = Mutex<HashMap<(PathBuf, String), VoicePack>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Load (and cache) a voice or blend from the npz voices file.
pub fn load_voice(voices_file: &Path, spec: &str) -> anyhow::Result<VoicePack> {
    let key = (voices_file.to_path_buf(), spec.to_string());
    if let Some(p) = cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("Kokoro voice cache lock poisoned"))?
        .get(&key)
    {
        return Ok(p.clone());
    }
    let parts = parse_blend(spec)?;
    let file = std::fs::File::open(voices_file)
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", voices_file.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let mut mixed: Option<Vec<f32>> = None;
    for (name, weight) in &parts {
        let mut entry = zip
            .by_name(&format!("{name}.npy"))
            .map_err(|_| anyhow::anyhow!("voice {name} not found in {}", voices_file.display()))?;
        anyhow::ensure!(
            entry.size() <= (PACK_ROWS * STYLE_DIM * 4 + 65_536) as u64,
            "voice {name} is too large"
        );
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        let (shape, data) = parse_npy_f32(&bytes)?;
        anyhow::ensure!(
            shape == [PACK_ROWS, 1, STYLE_DIM],
            "voice {name} has unexpected shape {shape:?}"
        );
        match &mut mixed {
            None => mixed = Some(data.iter().map(|v| v * weight).collect()),
            Some(acc) => {
                anyhow::ensure!(
                    acc.len() == data.len(),
                    "voice {name} has a different length"
                );
                for (a, v) in acc.iter_mut().zip(&data) {
                    *a += v * weight;
                }
            }
        }
    }
    let pack = VoicePack(Arc::new(mixed.expect("at least one voice")));
    cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("Kokoro voice cache lock poisoned"))?
        .insert(key, pack.clone());
    Ok(pack)
}

/// Names of every voice in the npz.
pub fn list_voice_file(voices_file: &Path) -> anyhow::Result<Vec<String>> {
    let zip = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(voices_file)?))?;
    Ok(zip
        .file_names()
        .filter_map(|n| n.strip_suffix(".npy"))
        .map(str::to_string)
        .collect())
}

/// Minimal .npy reader for little-endian float32 C-order arrays.
pub fn parse_npy_f32(bytes: &[u8]) -> anyhow::Result<(Vec<usize>, Vec<f32>)> {
    anyhow::ensure!(
        bytes.len() > 10 && &bytes[..6] == b"\x93NUMPY",
        "not an .npy file"
    );
    let major = bytes[6];
    anyhow::ensure!((1..=3).contains(&major), "unsupported .npy version {major}");
    let (header_len, start) = if major == 1 {
        (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10)
    } else {
        anyhow::ensure!(bytes.len() > 12, "truncated .npy");
        (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12,
        )
    };
    anyhow::ensure!(bytes.len() >= start + header_len, "truncated .npy header");
    let header = std::str::from_utf8(&bytes[start..start + header_len])?;
    anyhow::ensure!(
        header.contains("'<f4'"),
        "voice dtype must be <f4, header: {header}"
    );
    anyhow::ensure!(
        header.contains("'fortran_order': False"),
        "fortran-order arrays are not supported"
    );
    let shape_str = header
        .split("'shape':")
        .nth(1)
        .and_then(|s| s.split_once('(').map(|(_, r)| r))
        .and_then(|s| s.split_once(')').map(|(l, _)| l))
        .ok_or_else(|| anyhow::anyhow!("no shape in .npy header"))?;
    let shape: Vec<usize> = shape_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse())
        .collect::<Result<_, _>>()?;
    let count = shape
        .iter()
        .try_fold(1usize, |n, dim| n.checked_mul(*dim))
        .ok_or_else(|| anyhow::anyhow!(".npy shape overflows"))?;
    anyhow::ensure!(count > 0, "empty .npy array");
    let data = &bytes[start + header_len..];
    let byte_count = count
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!(".npy data size overflows"))?;
    anyhow::ensure!(data.len() == byte_count, "incorrect .npy data length");
    let values: Vec<f32> = data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    anyhow::ensure!(
        values.iter().all(|v| v.is_finite()),
        "non-finite .npy voice data"
    );
    Ok((shape, values))
}

/// Closest espeak voice for a Kokoro id (`am_*` -> US male, ...).
pub fn espeak_voice_for(spec: &str) -> &'static str {
    if ESPEAK_VOICES.iter().any(|(v, _)| *v == spec) {
        return ESPEAK_VOICES.iter().find(|(v, _)| *v == spec).unwrap().0;
    }
    let first = spec.split('+').next().unwrap_or("").trim();
    match (first.chars().next(), first.chars().nth(1)) {
        (Some('b'), Some('f')) => "en-gb+f3",
        (Some('b'), _) => "en-gb+m3",
        (_, Some('f')) => "en-us+f3",
        _ => "en-us+m3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(kokoro_label("am_michael"), "Michael (US male)");
        assert_eq!(kokoro_label("bf_emma"), "Emma (UK female)");
        assert_eq!(kokoro_label("af_heart"), "Heart (US female)");
    }

    #[test]
    fn blends() {
        let b = parse_blend("af_heart:0.6+am_michael:0.4").unwrap();
        assert_eq!(b[0].0, "af_heart");
        assert!((b[0].1 - 0.6).abs() < 1e-6 && (b[1].1 - 0.4).abs() < 1e-6);
        let eq = parse_blend("af_heart+am_michael").unwrap();
        assert!((eq[0].1 - 0.5).abs() < 1e-6);
        assert!(parse_blend("af_heart:-1").is_err());
        assert!(is_kokoro_spec("af_heart:3+bm_george:1"));
        assert!(!is_kokoro_spec("en-us+m3"));
        assert_eq!(espeak_voice_for("bf_emma"), "en-gb+f3");
        assert_eq!(espeak_voice_for("am_michael:1+af_heart:1"), "en-us+m3");
    }

    #[test]
    fn very_large_blend_weights_stay_normalized() {
        let blend = parse_blend("af_heart:3e38+am_michael:3e38").unwrap();
        assert_eq!(blend[0].1, 0.5);
        assert_eq!(blend[1].1, 0.5);
    }

    #[test]
    fn empty_pack_does_not_panic() {
        assert!(VoicePack(Arc::new(Vec::new())).style(1).is_empty());
    }

    #[test]
    fn npy_roundtrip() {
        let header = "{'descr': '<f4', 'fortran_order': False, 'shape': (2, 1, 256), }";
        let mut padded = header.to_string();
        while (10 + padded.len() + 1) % 64 != 0 {
            padded.push(' ');
        }
        padded.push('\n');
        let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
        bytes.extend_from_slice(&(padded.len() as u16).to_le_bytes());
        bytes.extend_from_slice(padded.as_bytes());
        for i in 0..512 {
            bytes.extend_from_slice(&(i as f32).to_le_bytes());
        }
        let (shape, data) = parse_npy_f32(&bytes).unwrap();
        assert_eq!(shape, vec![2, 1, 256]);
        assert_eq!(data[511], 511.0);
        let pack = VoicePack(Arc::new(data));
        assert_eq!(pack.style(1)[0], 0.0);
        assert_eq!(pack.style(2)[0], 256.0);
        assert_eq!(pack.style(999)[0], 256.0);
    }

    #[test]
    fn corrupt_npy_data_fails_without_panicking() {
        let npy = |header: &str, values: &[f32]| {
            let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
            bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
            bytes.extend_from_slice(header.as_bytes());
            for value in values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes
        };
        for bytes in [
            Vec::new(),
            b"\x93NUMPY\x01\x00\xff\xff".to_vec(),
            npy(
                "{'descr': '<f4', 'fortran_order': False, 'shape': (18446744073709551615, 256), }",
                &[],
            ),
            npy(
                "{'descr': '<f4', 'fortran_order': False, 'shape': (0, 1, 256), }",
                &[],
            ),
            npy(
                "{'descr': '<f4', 'fortran_order': False, 'shape': (1,), }",
                &[f32::NAN],
            ),
            npy(
                "{'descr': '<f4', 'fortran_order': False, 'shape': (2,), }",
                &[0.0],
            ),
            npy(
                "{'descr': '<f4', 'fortran_order': True, 'shape': (1,), }",
                &[0.0],
            ),
        ] {
            assert!(parse_npy_f32(&bytes).is_err());
        }
    }
}
