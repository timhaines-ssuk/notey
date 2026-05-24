//! Diarization wrapper around sherpa-onnx. Gated behind `feature = "ml"`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub cluster_id: i32,
}

#[derive(Debug, Clone)]
pub struct ClusterEmbedding {
    pub cluster_id: i32,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct DiarizationResult {
    pub segments: Vec<DiarSegment>,
    pub embeddings: Vec<ClusterEmbedding>,
}

#[cfg(feature = "ml")]
pub fn diarize(
    segmentation_model: &Path,
    embedding_model: &Path,
    audio_path: &Path,
    device_index: i32,
) -> Result<DiarizationResult> {
    use sherpa_rs::diarize::{Diarize, DiarizeConfig};

    let cfg = DiarizeConfig {
        num_clusters: None,
        threshold: Some(0.5),
        min_duration_on: Some(0.3),
        min_duration_off: Some(0.5),
        provider: Some(provider_name(device_index)),
        debug: false,
    };
    let mut diar = Diarize::new(segmentation_model, embedding_model, cfg)
        .map_err(|e| anyhow!("sherpa diarize init: {e}"))?;
    let samples = crate::transcribe::load_mono_16k(audio_path)?;
    let segs = diar
        .compute(samples.clone(), None)
        .map_err(|e| anyhow!("sherpa diarize compute: {e}"))?;

    let mut segments: Vec<DiarSegment> = segs
        .iter()
        .map(|s| DiarSegment {
            start_seconds: s.start as f64,
            end_seconds: s.end as f64,
            cluster_id: s.speaker,
        })
        .collect();
    segments.sort_by(|a, b| a.start_seconds.partial_cmp(&b.start_seconds).unwrap());

    // sherpa's diarize API doesn't return per-segment embeddings; compute one
    // per cluster from the audio with the embedding extractor directly.
    let embeddings = extract_cluster_embeddings(embedding_model, &segments, &samples, device_index)
        .unwrap_or_default();

    Ok(DiarizationResult { segments, embeddings })
}

#[cfg(not(feature = "ml"))]
pub fn diarize(
    _segmentation_model: &Path,
    _embedding_model: &Path,
    _audio_path: &Path,
    _device_index: i32,
) -> Result<DiarizationResult> {
    Err(anyhow!(
        "diarization disabled: rebuild with `--features ml` (or `--features cuda`)"
    ))
}

#[cfg(feature = "ml")]
fn provider_name(device_index: i32) -> String {
    if device_index >= 0 { "cuda".into() } else { "cpu".into() }
}

#[cfg(feature = "ml")]
fn extract_cluster_embeddings(
    embedding_model: &Path,
    segments: &[DiarSegment],
    samples: &[f32],
    device_index: i32,
) -> Result<Vec<ClusterEmbedding>> {
    use sherpa_rs::speaker_id::{EmbeddingExtractor, ExtractorConfig};

    let cfg = ExtractorConfig {
        model: embedding_model.to_string_lossy().into_owned(),
        num_threads: Some(1),
        debug: false,
        provider: Some(provider_name(device_index)),
        ..Default::default()
    };
    let mut extractor =
        EmbeddingExtractor::new(cfg).map_err(|e| anyhow!("embedding extractor: {e}"))?;

    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<i32, Vec<Vec<f32>>> = BTreeMap::new();

    for seg in segments {
        let start = (seg.start_seconds * 16_000.0) as usize;
        let end = ((seg.end_seconds * 16_000.0) as usize).min(samples.len());
        if end <= start || end - start < 16_000 / 4 {
            continue;
        }
        let slice = samples[start..end].to_vec();
        if let Ok(emb) = extractor.compute_speaker_embedding(slice, 16_000) {
            buckets.entry(seg.cluster_id).or_default().push(emb);
        }
    }

    let mut out = Vec::new();
    for (cluster_id, embs) in buckets {
        if embs.is_empty() {
            continue;
        }
        let dim = embs[0].len();
        let mut mean = vec![0.0f32; dim];
        for e in &embs {
            for (i, v) in e.iter().enumerate() {
                mean[i] += v;
            }
        }
        for v in mean.iter_mut() {
            *v /= embs.len() as f32;
        }
        out.push(ClusterEmbedding {
            cluster_id,
            embedding: mean,
        });
    }
    Ok(out)
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

pub fn merge_embedding(existing: &[f32], existing_count: u32, new: &[f32]) -> Vec<f32> {
    if existing.is_empty() {
        return new.to_vec();
    }
    let w = existing_count as f32;
    existing
        .iter()
        .zip(new.iter())
        .map(|(a, b)| (a * w + b) / (w + 1.0))
        .collect()
}

pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
