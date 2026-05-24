//! USB-recorder import pipeline (§4 INGEST → device branch).
//!
//! For each file off the device we:
//!   1. Open the source for read.
//!   2. Stream-copy into the data dir, computing a SHA-256 over what's read.
//!   3. Re-open the destination and stream-hash it.
//!   4. Compare hashes + file sizes. Only if both match do we treat the
//!      import as verified.
//!   5. The optional wipe step only deletes source files whose copies passed
//!      verification — failures stay on the device so nothing is lost.
//!
//! Imported recordings are mono — `auto_enroll_self` skips them.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const COPY_BUF_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub source_path: PathBuf,
    pub recording_id: Option<i64>,
    pub dest_path: Option<PathBuf>,
    pub bytes: u64,
    pub source_sha256: String,
    pub dest_sha256: String,
    pub verified: bool,
    pub error: Option<String>,
}

pub fn import_file(
    db: &std::sync::Mutex<rusqlite::Connection>,
    data_dir: &Path,
    source_path: &Path,
) -> ImportResult {
    match try_import_file(db, data_dir, source_path) {
        Ok(r) => r,
        Err(e) => ImportResult {
            source_path: source_path.to_path_buf(),
            recording_id: None,
            dest_path: None,
            bytes: 0,
            source_sha256: String::new(),
            dest_sha256: String::new(),
            verified: false,
            error: Some(format!("{e:#}")),
        },
    }
}

fn try_import_file(
    db: &std::sync::Mutex<rusqlite::Connection>,
    data_dir: &Path,
    source_path: &Path,
) -> Result<ImportResult> {
    let ext = source_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if !matches!(ext.as_str(), "wav") {
        anyhow::bail!(
            "unsupported format '.{}' — v1 only handles .wav from the device. \
             Convert with `ffmpeg -i in.{} out.wav` and import the .wav.",
            ext,
            ext
        );
    }

    let source_meta = std::fs::metadata(source_path)
        .with_context(|| format!("stat {}", source_path.display()))?;
    let expected_bytes = source_meta.len();

    let dest_dir = data_dir.join("audio");
    std::fs::create_dir_all(&dest_dir).ok();

    let filename = source_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("import_{}.wav", chrono::Utc::now().timestamp()));

    // Reserve a recording id for the destination filename.
    let rec_id = {
        let conn = db.lock().unwrap();
        crate::db::create_imported_recording(&conn, &filename, source_path)?
    };

    let dest = dest_dir.join(format!("import_{rec_id}_{filename}"));
    let dest_tmp = dest.with_extension("partial");

    // Stream-copy with running SHA-256 over the source bytes as they're read.
    let mut src_hash = Sha256::new();
    let bytes_copied = {
        let mut reader = std::fs::File::open(source_path)
            .with_context(|| format!("open source {}", source_path.display()))?;
        let mut writer = std::fs::File::create(&dest_tmp)
            .with_context(|| format!("create dest {}", dest_tmp.display()))?;
        let mut buf = vec![0u8; COPY_BUF_BYTES];
        let mut total: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            src_hash.update(&buf[..n]);
            writer.write_all(&buf[..n])?;
            total += n as u64;
        }
        writer.sync_all()?; // make Windows actually flush to disk
        total
    };
    let source_sha = format!("{:x}", src_hash.finalize());

    if bytes_copied != expected_bytes {
        let _ = std::fs::remove_file(&dest_tmp);
        anyhow::bail!(
            "size mismatch: expected {expected_bytes} bytes, copied {bytes_copied}"
        );
    }

    // Independently hash what's actually on disk now.
    let dest_sha = sha256_file(&dest_tmp)
        .with_context(|| format!("hash dest {}", dest_tmp.display()))?;
    let dest_size = std::fs::metadata(&dest_tmp)?.len();

    if dest_sha != source_sha || dest_size != expected_bytes {
        let _ = std::fs::remove_file(&dest_tmp);
        // Mark the DB row as failed so the user sees it.
        if let Ok(conn) = db.lock() {
            let _ = crate::db::update_status(&conn, rec_id, "failed");
        }
        anyhow::bail!(
            "verification failed: source sha256 {} vs dest sha256 {}, dest size {} vs expected {}",
            source_sha,
            dest_sha,
            dest_size,
            expected_bytes
        );
    }

    // Atomic rename now that we know the data is good.
    std::fs::rename(&dest_tmp, &dest)
        .with_context(|| format!("rename {} → {}", dest_tmp.display(), dest.display()))?;

    {
        let conn = db.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET audio_path = ?1, status = 'transcribing' WHERE id = ?2",
            rusqlite::params![dest.to_string_lossy(), rec_id],
        )?;
    }

    Ok(ImportResult {
        source_path: source_path.to_path_buf(),
        recording_id: Some(rec_id),
        dest_path: Some(dest),
        bytes: bytes_copied,
        source_sha256: source_sha,
        dest_sha256: dest_sha,
        verified: true,
        error: None,
    })
}

fn sha256_file(p: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; COPY_BUF_BYTES];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Safe wipe: only delete files that were verified, and only if they live
/// under the canonicalised mount path.
pub fn wipe_verified(mount: &Path, verified_sources: &[PathBuf]) -> Result<usize> {
    let canon_mount = std::fs::canonicalize(mount).unwrap_or_else(|_| mount.to_path_buf());
    let mut removed = 0;
    for f in verified_sources {
        let canon_f = std::fs::canonicalize(f).unwrap_or_else(|_| f.clone());
        if !canon_f.starts_with(&canon_mount) {
            return Err(anyhow!(
                "refusing to delete {} — not under mount {}",
                canon_f.display(),
                canon_mount.display()
            ));
        }
        match std::fs::remove_file(f) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!("failed to remove {}: {e}", f.display()),
        }
    }
    Ok(removed)
}
