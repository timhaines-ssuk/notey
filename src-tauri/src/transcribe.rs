//! Whisper wrapper. Gated behind the `ml` feature so the crate still compiles
//! before the user installs clang/cmake/MSVC. Without `ml`, calls return a
//! clear runtime error and the rest of the app continues to function.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribedSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub confidence: Option<f64>,
    pub channel: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Live,
    Finalize,
}

#[cfg(feature = "ml")]
pub fn transcribe_file(
    model_path: &Path,
    audio_path: &Path,
    _mode: Mode,
    device_index: i32,
) -> Result<Vec<TranscribedSegment>> {
    let samples = load_mono_16k(audio_path)?;
    transcribe_samples(model_path, &samples, _mode, device_index)
}

#[cfg(feature = "ml")]
pub fn transcribe_samples(
    model_path: &Path,
    samples: &[f32],
    _mode: Mode,
    device_index: i32,
) -> Result<Vec<TranscribedSegment>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let mut ctx_params = WhisperContextParameters::default();
    ctx_params.use_gpu(device_index >= 0);

    let ctx = WhisperContext::new_with_params(
        model_path.to_str().ok_or_else(|| anyhow!("non-utf8 model path"))?,
        ctx_params,
    )
    .map_err(|e| anyhow!("whisper init: {e}"))?;

    let mut state = ctx.create_state().map_err(|e| anyhow!("whisper state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, samples).map_err(|e| anyhow!("whisper full: {e}"))?;

    let n_segments = state.full_n_segments();
    let mut out = Vec::with_capacity(n_segments as usize);
    for i in 0..n_segments {
        let seg = state.get_segment(i).ok_or_else(|| anyhow!("segment {i} out of bounds"))?;
        let text = seg.to_str_lossy().map_err(|e| anyhow!("{e}"))?.into_owned();
        let cleaned = strip_artifacts(&text);
        if cleaned.trim().is_empty() {
            continue;
        }
        let t0 = seg.start_timestamp() as f64 / 100.0;
        let t1 = seg.end_timestamp() as f64 / 100.0;
        out.push(TranscribedSegment {
            start_seconds: t0,
            end_seconds: t1,
            text: cleaned,
            confidence: None,
            channel: None,
        });
    }
    Ok(out)
}

#[cfg(not(feature = "ml"))]
pub fn transcribe_file(
    _model_path: &Path,
    _audio_path: &Path,
    _mode: Mode,
    _device_index: i32,
) -> Result<Vec<TranscribedSegment>> {
    Err(anyhow!(
        "transcription disabled: rebuild with `--features ml` (or `--features cuda`)"
    ))
}

#[cfg(not(feature = "ml"))]
pub fn transcribe_samples(
    _model_path: &Path,
    _samples: &[f32],
    _mode: Mode,
    _device_index: i32,
) -> Result<Vec<TranscribedSegment>> {
    Err(anyhow!(
        "transcription disabled: rebuild with `--features ml` (or `--features cuda`)"
    ))
}

pub fn load_mono_16k(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let src_sr = spec.sample_rate;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|s| s as f32 / (i16::MAX as f32))
            .collect(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mono: Vec<f32> = if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if src_sr == 16_000 {
        Ok(mono)
    } else {
        Ok(crate::audio_capture::linear_resample(&mono, src_sr, 16_000))
    }
}

/// Load both mic (left) and loopback (right) channels separately from a
/// stereo-mic-loopback WAV (the format `audio_capture` writes for calls).
pub fn load_stereo_channels_16k(path: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels != 2 {
        // Mono fallback: same signal in both channels.
        let mono = load_mono_16k(path)?;
        return Ok((mono.clone(), mono));
    }
    let src_sr = spec.sample_rate;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|s| s as f32 / (i16::MAX as f32))
            .collect(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mut left = Vec::with_capacity(samples.len() / 2);
    let mut right = Vec::with_capacity(samples.len() / 2);
    for f in samples.chunks_exact(2) {
        left.push(f[0]);
        right.push(f[1]);
    }
    if src_sr != 16_000 {
        left = crate::audio_capture::linear_resample(&left, src_sr, 16_000);
        right = crate::audio_capture::linear_resample(&right, src_sr, 16_000);
    }
    Ok((left, right))
}

fn strip_artifacts(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}
