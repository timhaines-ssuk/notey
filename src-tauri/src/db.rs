use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS recordings (
    id INTEGER PRIMARY KEY,
    source_filename TEXT,
    source TEXT NOT NULL,
    app_name TEXT,
    channel_layout TEXT,
    imported_at TEXT NOT NULL,
    duration_seconds REAL,
    status TEXT NOT NULL,
    audio_deleted INTEGER NOT NULL DEFAULT 0,
    audio_path TEXT
);

CREATE TABLE IF NOT EXISTS speakers (
    id INTEGER PRIMARY KEY,
    name TEXT,
    embedding BLOB,
    sample_count INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    is_self INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS clusters (
    id INTEGER PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    local_id INTEGER NOT NULL,
    embedding BLOB,
    speaker_id INTEGER REFERENCES speakers(id),
    UNIQUE(recording_id, local_id)
);

CREATE TABLE IF NOT EXISTS segments (
    id INTEGER PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    cluster_id INTEGER REFERENCES clusters(id),
    speaker_id INTEGER REFERENCES speakers(id),
    start_seconds REAL NOT NULL,
    end_seconds REAL NOT NULL,
    text TEXT NOT NULL,
    confidence REAL
);

CREATE INDEX IF NOT EXISTS idx_segments_recording ON segments(recording_id);

CREATE TABLE IF NOT EXISTS summary_chunks (
    id INTEGER PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    start_seconds REAL NOT NULL,
    end_seconds REAL NOT NULL,
    text TEXT NOT NULL,
    generated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rolling_summary (
    recording_id INTEGER PRIMARY KEY REFERENCES recordings(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    through_seconds REAL NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS summaries (
    recording_id INTEGER PRIMARY KEY REFERENCES recordings(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    model TEXT NOT NULL,
    generated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS exports (
    id INTEGER PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    format TEXT NOT NULL,
    file_path TEXT NOT NULL,
    exported_at TEXT NOT NULL,
    speakers_confirmed INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
    conn.execute_batch(SCHEMA)?;
    seed_defaults(&conn)?;
    Ok(conn)
}

fn seed_defaults(conn: &Connection) -> Result<()> {
    let defaults = [
        ("transcribe_live", r#"{"model":"small.en","backend":"cuda","device_index":0}"#),
        ("transcribe_finalize", r#"{"model":"medium.en","backend":"cuda","device_index":0}"#),
        ("diarize", r#"{"backend":"cuda","device_index":0}"#),
        ("summarize", r#"{"model":"qwen2.5:7b","backend":"cuda"}"#),
        ("ollama_url", "http://localhost:11434"),
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES (?1, ?2)",
            params![k, v],
        )?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecordingRow {
    pub id: i64,
    pub source_filename: Option<String>,
    pub source: String,
    pub app_name: Option<String>,
    pub imported_at: String,
    pub duration_seconds: Option<f64>,
    pub status: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SegmentRow {
    pub id: i64,
    pub speaker_id: Option<i64>,
    pub speaker_name: Option<String>,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub confidence: Option<f64>,
}

pub fn list_recordings(conn: &Connection) -> Result<Vec<RecordingRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_filename, source, app_name, imported_at, duration_seconds, status
         FROM recordings ORDER BY imported_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RecordingRow {
                id: r.get(0)?,
                source_filename: r.get(1)?,
                source: r.get(2)?,
                app_name: r.get(3)?,
                imported_at: r.get(4)?,
                duration_seconds: r.get(5)?,
                status: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_segments(conn: &Connection, recording_id: i64) -> Result<Vec<SegmentRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.speaker_id, sp.name, s.start_seconds, s.end_seconds, s.text, s.confidence
         FROM segments s LEFT JOIN speakers sp ON sp.id = s.speaker_id
         WHERE s.recording_id = ?1 ORDER BY s.start_seconds ASC",
    )?;
    let rows = stmt
        .query_map(params![recording_id], |r| {
            Ok(SegmentRow {
                id: r.get(0)?,
                speaker_id: r.get(1)?,
                speaker_name: r.get(2)?,
                start_seconds: r.get(3)?,
                end_seconds: r.get(4)?,
                text: r.get(5)?,
                confidence: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn create_call_recording(conn: &Connection, data_dir: &Path) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO recordings(source, app_name, channel_layout, imported_at, status, audio_path)
         VALUES ('call', NULL, 'stereo_mic_loopback', ?1, 'recording', ?2)",
        params![now, data_dir.join("audio").to_string_lossy()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn create_imported_recording(
    conn: &Connection,
    filename: &str,
    audio_path: &Path,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO recordings(source_filename, source, channel_layout, imported_at, status, audio_path)
         VALUES (?1, 'device', 'mono', ?2, 'transcribing', ?3)",
        params![filename, now, audio_path.to_string_lossy()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute("UPDATE recordings SET status = ?1 WHERE id = ?2", params![status, id])?;
    Ok(())
}

pub fn mark_audio_deleted(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("UPDATE recordings SET audio_deleted = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn insert_segment(
    conn: &Connection,
    recording_id: i64,
    cluster_id: Option<i64>,
    speaker_id: Option<i64>,
    start: f64,
    end: f64,
    text: &str,
    confidence: Option<f64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO segments(recording_id, cluster_id, speaker_id, start_seconds, end_seconds, text, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![recording_id, cluster_id, speaker_id, start, end, text, confidence],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn upsert_cluster(
    conn: &Connection,
    recording_id: i64,
    local_id: i64,
    embedding: &[u8],
) -> Result<i64> {
    conn.execute(
        "INSERT INTO clusters(recording_id, local_id, embedding) VALUES (?1, ?2, ?3)
         ON CONFLICT(recording_id, local_id) DO UPDATE SET embedding = excluded.embedding",
        params![recording_id, local_id, embedding],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM clusters WHERE recording_id = ?1 AND local_id = ?2",
        params![recording_id, local_id],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn get_settings(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?)
}
