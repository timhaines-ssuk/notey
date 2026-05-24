use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub struct WhisperModel {
    pub name: &'static str,
    pub size_mb: u32,
    pub sha256: Option<&'static str>,
}

pub const WHISPER_MODELS: &[WhisperModel] = &[
    WhisperModel { name: "tiny.en",         size_mb: 75,   sha256: None },
    WhisperModel { name: "tiny",            size_mb: 75,   sha256: None },
    WhisperModel { name: "base.en",         size_mb: 142,  sha256: None },
    WhisperModel { name: "base",            size_mb: 142,  sha256: None },
    WhisperModel { name: "small.en",        size_mb: 466,  sha256: None },
    WhisperModel { name: "small",           size_mb: 466,  sha256: None },
    WhisperModel { name: "medium.en",       size_mb: 1500, sha256: None },
    WhisperModel { name: "medium",          size_mb: 1500, sha256: None },
    WhisperModel { name: "large-v3",        size_mb: 3100, sha256: None },
    WhisperModel { name: "large-v3-turbo",  size_mb: 1600, sha256: None },
];

pub fn whisper_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

pub fn whisper_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join("whisper").join(format!("ggml-{name}.bin"))
}

pub fn sherpa_paths(models_dir: &Path) -> (PathBuf, PathBuf) {
    let dir = models_dir.join("sherpa");
    (
        dir.join("sherpa-onnx-pyannote-segmentation-3-0").join("model.onnx"),
        dir.join("3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"),
    )
}

pub const SEGMENTATION_TARBALL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
pub const EMBEDDING_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";

pub async fn ensure_sherpa(models_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let (seg, emb) = sherpa_paths(models_dir);
    if !emb.exists() {
        download(EMBEDDING_URL, &emb).await.context("downloading embedding model")?;
    }
    if !seg.exists() {
        let tarball = models_dir
            .join("sherpa")
            .join("sherpa-onnx-pyannote-segmentation-3-0.tar.bz2");
        download(SEGMENTATION_TARBALL_URL, &tarball)
            .await
            .context("downloading segmentation tarball")?;
        extract_tar_bz2(&tarball, &models_dir.join("sherpa"))?;
        let _ = std::fs::remove_file(&tarball);
    }
    Ok((seg, emb))
}

#[derive(serde::Serialize, Debug, Clone)]
pub struct DownloadProgress {
    pub label: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub done: bool,
}

pub async fn download_with_progress<F: Fn(DownloadProgress) + Send + 'static>(
    url: &str,
    dest: &Path,
    label: &str,
    cb: F,
) -> Result<()> {
    if dest.exists() {
        cb(DownloadProgress { label: label.into(), bytes: 0, total: None, done: true });
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = dest.with_extension("partial");
    let client = reqwest::Client::builder().user_agent("notetaker/0.1").build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let mut bytes: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        bytes += chunk.len() as u64;
        file.write_all(&chunk).await?;
        cb(DownloadProgress { label: label.into(), bytes, total, done: false });
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, dest).await?;
    cb(DownloadProgress { label: label.into(), bytes, total, done: true });
    Ok(())
}

pub fn extract_tar_bz2_pub(tarball: &Path, into: &Path) -> Result<()> {
    extract_tar_bz2(tarball, into)
}

fn extract_tar_bz2(tarball: &Path, into: &Path) -> Result<()> {
    // sherpa-onnx ships the segmentation model as .tar.bz2. To avoid pulling a
    // bz2 + tar crate combo for this single use, we shell out to `tar` (built
    // into Windows 10/11 since 1803).
    let status = std::process::Command::new("tar")
        .arg("-xjf")
        .arg(tarball)
        .arg("-C")
        .arg(into)
        .status()
        .context("failed to spawn `tar` to extract segmentation model")?;
    if !status.success() {
        anyhow::bail!("tar extract failed for {}", tarball.display());
    }
    Ok(())
}

pub async fn download(url: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = dest.with_extension("partial");
    let client = reqwest::Client::builder()
        .user_agent("notetaker/0.1")
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

pub async fn ensure_whisper(models_dir: &Path, name: &str) -> Result<PathBuf> {
    let path = whisper_path(models_dir, name);
    if !path.exists() {
        download(&whisper_url(name), &path)
            .await
            .with_context(|| format!("downloading whisper model {name}"))?;
    }
    Ok(path)
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let got = format!("{:x}", hasher.finalize());
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(anyhow!("checksum mismatch for {}: got {got}, expected {expected}", path.display()))
    }
}
