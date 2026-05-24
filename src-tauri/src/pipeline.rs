use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ModelConfig {
    pub whisper_finalize_path: PathBuf,
    pub diarize_segmentation_path: PathBuf,
    pub diarize_embedding_path: PathBuf,
    pub device_index: i32,
}

pub async fn resolve_config(db: &Mutex<Connection>, data_dir: &Path) -> Result<ModelConfig> {
    let (finalize_name, backend) = {
        let conn = db.lock().unwrap();
        let raw = crate::db::get_setting(&conn, "transcribe_finalize")?
            .ok_or_else(|| anyhow!("transcribe_finalize setting missing"))?;
        let v: serde_json::Value = serde_json::from_str(&raw)?;
        let model = v["model"].as_str().unwrap_or("medium.en").to_string();
        let backend = v["backend"].as_str().unwrap_or("cpu").to_string();
        (model, backend)
    };
    let models_dir = data_dir.join("models");
    let whisper_finalize_path = crate::models::ensure_whisper(&models_dir, &finalize_name).await?;
    let (seg, emb) = crate::models::ensure_sherpa(&models_dir).await?;
    let device_index = if backend == "cuda" { 0 } else { -1 };
    Ok(ModelConfig {
        whisper_finalize_path,
        diarize_segmentation_path: seg,
        diarize_embedding_path: emb,
        device_index,
    })
}

pub fn run_finalize(
    db: &Mutex<Connection>,
    recording_id: i64,
    audio_path: &Path,
    cfg: &ModelConfig,
) -> Result<()> {
    {
        let c = db.lock().unwrap();
        crate::db::update_status(&c, recording_id, "transcribing")?;
    }
    let segments = crate::transcribe::transcribe_file(
        &cfg.whisper_finalize_path,
        audio_path,
        crate::transcribe::Mode::Finalize,
        cfg.device_index,
    )?;

    {
        let c = db.lock().unwrap();
        crate::db::update_status(&c, recording_id, "diarizing")?;
    }
    let diar = crate::diarize::diarize(
        &cfg.diarize_segmentation_path,
        &cfg.diarize_embedding_path,
        audio_path,
        cfg.device_index,
    )?;

    let mut conn = db.lock().unwrap();

    // Clear any prior live rows for this recording — we now have authoritative
    // finalize-pass segments.
    conn.execute(
        "DELETE FROM segments WHERE recording_id = ?1",
        rusqlite::params![recording_id],
    )?;
    conn.execute(
        "DELETE FROM clusters WHERE recording_id = ?1",
        rusqlite::params![recording_id],
    )?;

    let tx = conn.unchecked_transaction()?;

    let mut cluster_db_ids = std::collections::HashMap::<i32, i64>::new();
    for emb in &diar.embeddings {
        let bytes = crate::diarize::encode_embedding(&emb.embedding);
        let cid = crate::db::upsert_cluster(&tx, recording_id, emb.cluster_id as i64, &bytes)?;
        cluster_db_ids.insert(emb.cluster_id, cid);
    }
    // Also upsert clusters that had segments but no embedding (shouldn't happen normally).
    let mut seen = std::collections::HashSet::<i32>::new();
    for s in &diar.segments {
        seen.insert(s.cluster_id);
    }
    for cid in seen {
        cluster_db_ids
            .entry(cid)
            .or_insert_with(|| crate::db::upsert_cluster(&tx, recording_id, cid as i64, &[]).unwrap_or(0));
    }

    for s in &segments {
        let cluster_id = best_cluster(&diar.segments, s.start_seconds, s.end_seconds)
            .and_then(|c| cluster_db_ids.get(&c).copied());
        crate::db::insert_segment(
            &tx,
            recording_id,
            cluster_id,
            None,
            s.start_seconds,
            s.end_seconds,
            &s.text,
            s.confidence,
        )?;
    }

    tx.commit()?;
    crate::db::update_status(&conn, recording_id, "awaiting_naming")?;
    Ok(())
}

fn best_cluster(diar_segments: &[crate::diarize::DiarSegment], start: f64, end: f64) -> Option<i32> {
    let mut best: Option<(i32, f64)> = None;
    for d in diar_segments {
        let overlap = (end.min(d.end_seconds) - start.max(d.start_seconds)).max(0.0);
        if overlap <= 0.0 {
            continue;
        }
        match best {
            Some((_, prev)) if prev >= overlap => {}
            _ => best = Some((d.cluster_id, overlap)),
        }
    }
    best.map(|(c, _)| c)
}

/// Stereo call recordings have the user's mic on the left channel. Whatever
/// cluster sherpa picked for the mic channel is the user — find the
/// cluster whose segments contain mostly mic-channel energy, and bind that
/// cluster to the "Self" speaker.
pub fn auto_enroll_self(
    db: &Mutex<Connection>,
    recording_id: i64,
    audio_path: &Path,
    cfg: &ModelConfig,
) -> Result<()> {
    let is_call = {
        let c = db.lock().unwrap();
        let row: Option<String> = c
            .query_row(
                "SELECT channel_layout FROM recordings WHERE id = ?1",
                rusqlite::params![recording_id],
                |r| r.get(0),
            )
            .ok();
        matches!(row.as_deref(), Some("stereo_mic_loopback"))
    };
    if !is_call {
        return Ok(());
    }

    let (mic, _loop) = crate::transcribe::load_stereo_channels_16k(audio_path)?;
    // Find cluster with highest mean RMS energy in mic channel.
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT cluster_id, start_seconds, end_seconds FROM segments
         WHERE recording_id = ?1 AND cluster_id IS NOT NULL",
    )?;
    let rows: Vec<(i64, f64, f64)> = stmt
        .query_map(rusqlite::params![recording_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut energy = std::collections::HashMap::<i64, (f64, f64)>::new(); // sum, count
    for (cluster_id, start, end) in rows {
        let a = (start * 16_000.0) as usize;
        let b = ((end * 16_000.0) as usize).min(mic.len());
        if b <= a {
            continue;
        }
        let rms = rms(&mic[a..b]);
        let e = energy.entry(cluster_id).or_insert((0.0, 0.0));
        e.0 += rms as f64 * (b - a) as f64;
        e.1 += (b - a) as f64;
    }
    let mic_cluster = energy
        .into_iter()
        .map(|(c, (s, n))| (c, if n > 0.0 { s / n } else { 0.0 }))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let Some((mic_cluster_id, _)) = mic_cluster else {
        return Ok(());
    };

    // Look for an existing "Self" speaker, else create one.
    let self_id: i64 = match conn.query_row(
        "SELECT id FROM speakers WHERE is_self = 1 LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(id) => id,
        Err(_) => {
            let cluster_emb: Vec<u8> = conn
                .query_row(
                    "SELECT COALESCE(embedding, X'') FROM clusters WHERE id = ?1",
                    rusqlite::params![mic_cluster_id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO speakers(name, embedding, sample_count, created_at, is_self)
                 VALUES ('You', ?1, 1, ?2, 1)",
                rusqlite::params![cluster_emb, now],
            )?;
            conn.last_insert_rowid()
        }
    };

    conn.execute(
        "UPDATE clusters SET speaker_id = ?1 WHERE id = ?2",
        rusqlite::params![self_id, mic_cluster_id],
    )?;
    conn.execute(
        "UPDATE segments SET speaker_id = ?1 WHERE cluster_id = ?2",
        rusqlite::params![self_id, mic_cluster_id],
    )?;
    // Don't ignore device-index, but cfg isn't used here except to signal CUDA;
    // future: re-extract Self embedding on each call to keep it tight.
    let _ = cfg;
    Ok(())
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}
