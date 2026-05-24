use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::diarize::{cosine_similarity, decode_embedding, encode_embedding, merge_embedding};

#[allow(dead_code)]
const SIM_PREFILL: f32 = 0.75; // above this, auto-pre-fill the name (frontend uses this hint via suggestions)
const SIM_SUGGEST: f32 = 0.55; // above this, suggest as candidate

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub audio_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub speaker_id: i64,
    pub name: String,
    pub similarity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnnamedCluster {
    pub cluster_id: i64,
    pub snippets: Vec<Snippet>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Decision {
    New { name: String },
    Existing { #[serde(rename = "speakerId")] speaker_id: i64 },
    Merge { #[serde(rename = "intoClusterId")] into_cluster_id: i64 },
    Noise,
}

pub fn unnamed_clusters(conn: &Connection, recording_id: i64) -> Result<Vec<UnnamedCluster>> {
    let mut cluster_stmt = conn.prepare(
        "SELECT id, embedding FROM clusters WHERE recording_id = ?1 AND speaker_id IS NULL",
    )?;
    let clusters: Vec<(i64, Vec<u8>)> = cluster_stmt
        .query_map(params![recording_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1).unwrap_or_default()))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let known: Vec<(i64, String, Vec<u8>)> = {
        let mut s = conn.prepare(
            "SELECT id, COALESCE(name, ''), COALESCE(embedding, X'') FROM speakers WHERE name IS NOT NULL",
        )?;
        let v = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };

    let mut out = Vec::new();
    for (cluster_id, emb_bytes) in clusters {
        let emb = decode_embedding(&emb_bytes);
        let mut sugg: Vec<Suggestion> = known
            .iter()
            .filter_map(|(sid, name, sb)| {
                let sim = cosine_similarity(&emb, &decode_embedding(sb));
                if sim >= SIM_SUGGEST {
                    Some(Suggestion {
                        speaker_id: *sid,
                        name: name.clone(),
                        similarity: sim,
                    })
                } else {
                    None
                }
            })
            .collect();
        sugg.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());
        sugg.truncate(5);

        let snippets = pick_snippets(conn, recording_id, cluster_id)?;

        out.push(UnnamedCluster {
            cluster_id,
            snippets,
            suggestions: sugg,
        });
    }
    Ok(out)
}

fn pick_snippets(conn: &Connection, recording_id: i64, cluster_id: i64) -> Result<Vec<Snippet>> {
    // §5: prefer 4–8s isolated, substantive segments, spread across recording.
    let mut s = conn.prepare(
        "SELECT start_seconds, end_seconds, text
         FROM segments
         WHERE recording_id = ?1 AND cluster_id = ?2
           AND (end_seconds - start_seconds) BETWEEN 2 AND 12
           AND length(text) > 12
         ORDER BY start_seconds",
    )?;
    let all: Vec<Snippet> = s
        .query_map(params![recording_id, cluster_id], |r| {
            Ok(Snippet {
                start: r.get(0)?,
                end: r.get(1)?,
                text: r.get::<_, String>(2)?.trim().to_string(),
                audio_b64: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if all.is_empty() {
        return Ok(vec![]);
    }
    // Take ~5 spread evenly through the cluster's segments.
    let want = 5.min(all.len());
    let mut picks = Vec::with_capacity(want);
    for i in 0..want {
        let idx = (i * all.len()) / want;
        picks.push(all[idx].clone());
    }
    Ok(picks)
}

pub fn confirm(
    conn: &Connection,
    _recording_id: i64,
    cluster_id: i64,
    decision: Decision,
) -> Result<()> {
    let cluster_emb_bytes: Vec<u8> = conn
        .query_row(
            "SELECT COALESCE(embedding, X'') FROM clusters WHERE id = ?1",
            params![cluster_id],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let cluster_emb = decode_embedding(&cluster_emb_bytes);

    match decision {
        Decision::New { name } => {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO speakers(name, embedding, sample_count, created_at)
                 VALUES (?1, ?2, 1, ?3)",
                params![name, encode_embedding(&cluster_emb), now],
            )?;
            let sid = conn.last_insert_rowid();
            attach_cluster(conn, cluster_id, sid)?;
        }
        Decision::Existing { speaker_id } => {
            let (existing_emb, count): (Vec<u8>, u32) = conn.query_row(
                "SELECT COALESCE(embedding, X''), sample_count FROM speakers WHERE id = ?1",
                params![speaker_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let merged = merge_embedding(&decode_embedding(&existing_emb), count, &cluster_emb);
            conn.execute(
                "UPDATE speakers SET embedding = ?1, sample_count = sample_count + 1 WHERE id = ?2",
                params![encode_embedding(&merged), speaker_id],
            )?;
            attach_cluster(conn, cluster_id, speaker_id)?;
        }
        Decision::Merge { into_cluster_id } => {
            let other_speaker: Option<i64> = conn
                .query_row(
                    "SELECT speaker_id FROM clusters WHERE id = ?1",
                    params![into_cluster_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(sid) = other_speaker {
                attach_cluster(conn, cluster_id, sid)?;
            } else {
                conn.execute(
                    "UPDATE segments SET cluster_id = ?1 WHERE cluster_id = ?2",
                    params![into_cluster_id, cluster_id],
                )?;
                conn.execute("DELETE FROM clusters WHERE id = ?1", params![cluster_id])?;
            }
        }
        Decision::Noise => {
            conn.execute(
                "DELETE FROM segments WHERE cluster_id = ?1",
                params![cluster_id],
            )?;
            conn.execute("DELETE FROM clusters WHERE id = ?1", params![cluster_id])?;
        }
    }
    Ok(())
}

fn attach_cluster(conn: &Connection, cluster_id: i64, speaker_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE clusters SET speaker_id = ?1 WHERE id = ?2",
        params![speaker_id, cluster_id],
    )?;
    conn.execute(
        "UPDATE segments SET speaker_id = ?1 WHERE cluster_id = ?2",
        params![speaker_id, cluster_id],
    )?;
    Ok(())
}
