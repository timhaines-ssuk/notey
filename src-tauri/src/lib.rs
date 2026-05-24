pub mod audio_capture;
pub mod call_detector;
pub mod db;
pub mod device_watcher;
pub mod diarize;
pub mod export;
pub mod hardware;
pub mod ingest;
pub mod live;
pub mod models;
pub mod monitor;
pub mod pipeline;
#[cfg(target_os = "windows")]
pub mod proc_loopback;
pub mod speakers;
pub mod summarize;
pub mod transcribe;

use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub data_dir: PathBuf,
    pub capture: Mutex<Option<audio_capture::CaptureHandle>>,
    pub active_recording: Mutex<Option<i64>>,
    pub hardware_cache: Mutex<Option<hardware::HardwareProfile>>,
    pub last_capture_error: Mutex<Option<String>>,
    pub live_worker: Mutex<Option<live::LiveHandle>>,
    pub monitor: Mutex<Option<monitor::MonitorHandle>>,
    /// Most recent pipeline-stage event ("transcribing", "complete", ...).
    /// Set whenever `run_pipeline` emits a `pipeline-stage` event so the UI
    /// can query state directly instead of relying only on event ordering.
    pub last_pipeline_stage: Mutex<Option<String>>,
}

#[tauri::command]
fn detect_hardware(state: tauri::State<AppState>) -> Result<hardware::HardwareProfile, String> {
    if let Some(cached) = state.hardware_cache.lock().unwrap().clone() {
        return Ok(cached);
    }
    let hw = hardware::detect().map_err(|e| e.to_string())?;
    *state.hardware_cache.lock().unwrap() = Some(hw.clone());
    Ok(hw)
}

#[tauri::command]
fn redetect_hardware(state: tauri::State<AppState>) -> Result<hardware::HardwareProfile, String> {
    let hw = hardware::detect().map_err(|e| e.to_string())?;
    *state.hardware_cache.lock().unwrap() = Some(hw.clone());
    Ok(hw)
}

#[tauri::command]
fn list_recordings(state: tauri::State<AppState>) -> Result<Vec<db::RecordingRow>, String> {
    let conn = state.db.lock().unwrap();
    db::list_recordings(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_segments(
    state: tauri::State<AppState>,
    recording_id: i64,
) -> Result<Vec<db::SegmentRow>, String> {
    let conn = state.db.lock().unwrap();
    db::get_segments(&conn, recording_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_unnamed_clusters(
    state: tauri::State<AppState>,
    recording_id: i64,
) -> Result<Vec<speakers::UnnamedCluster>, String> {
    let conn = state.db.lock().unwrap();
    speakers::unnamed_clusters(&conn, recording_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn confirm_speaker(
    state: tauri::State<AppState>,
    recording_id: i64,
    cluster_id: i64,
    decision: speakers::Decision,
) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    speakers::confirm(&conn, recording_id, cluster_id, decision).map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_call_capture(app: tauri::AppHandle) -> Result<i64, String> {
    let state = app.state::<AppState>();
    // Free up the cpal devices for the real capture path.
    *state.monitor.lock().unwrap() = None;
    *state.last_pipeline_stage.lock().unwrap() = None;
    *state.last_capture_error.lock().unwrap() = None;
    let (mic_name, loop_source, loop_name, live_model_name, backend) = {
        let conn = state.db.lock().unwrap();
        let mic_name = db::get_setting(&conn, "device_mic").ok().flatten().filter(|s| !s.is_empty());
        let loop_source = db::get_setting(&conn, "loopback_source").ok().flatten().unwrap_or_default();
        let loop_name = db::get_setting(&conn, "device_loopback").ok().flatten().filter(|s| !s.is_empty());
        let raw = db::get_setting(&conn, "transcribe_live").ok().flatten().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
        let live_model_name = v["model"].as_str().unwrap_or("small.en").to_string();
        let backend = v["backend"].as_str().unwrap_or("cpu").to_string();
        (mic_name, loop_source, loop_name, live_model_name, backend)
    };
    let rec_id = {
        let conn = state.db.lock().unwrap();
        db::create_call_recording(&conn, &state.data_dir).map_err(|e| e.to_string())?
    };

    let loopback = resolve_loopback(&loop_source, loop_name)?;
    let path = state.data_dir.join("audio").join(format!("call_{rec_id}.wav"));

    // Set up live transcription IF the live whisper model is already on disk.
    // We don't block capture-start on a multi-GB download — if the model
    // isn't present, capture proceeds without live transcription and the
    // finalize pass still produces full output on Stop.
    let models_dir = state.data_dir.join("models");
    let live_model_path = models::whisper_path(&models_dir, &live_model_name);
    let (live_buffer, live_handle) = if live_model_path.exists() {
        let buf = live::LiveBuffer::new(16_000);
        let buf_for_capture = buf.clone();
        let device_index = if backend == "cuda" { 0 } else { -1 };
        let app_clone = app.clone();
        let db_owned = std::sync::Arc::new(()); // dummy to align lifetime
        let _ = db_owned;
        // The worker closure captures `app` for the Tauri emit + needs DB access via state.
        let on_segment = move |seg: transcribe::TranscribedSegment| {
            let app = app_clone.clone();
            // Insert into DB and emit an event.
            let state = app.state::<AppState>();
            let conn = state.db.lock().unwrap();
            let _ = db::insert_segment(
                &conn,
                rec_id,
                None,
                None,
                seg.start_seconds,
                seg.end_seconds,
                &seg.text,
                seg.confidence,
            );
            drop(conn);
            let _ = app.emit("live-segment", (rec_id, &seg));
        };
        let handle = live::spawn_live(buf.clone(), live_model_path, device_index, on_segment);
        tracing::info!("live transcription enabled with model {live_model_name}");
        (Some(buf_for_capture), Some(handle))
    } else {
        tracing::info!(
            "live transcription disabled: model {live_model_name} not present at {}; \
             run the model downloader and try again",
            live_model_path.display()
        );
        let _ = app.emit(
            "live-status",
            format!("Live transcription off: model '{live_model_name}' not downloaded yet"),
        );
        (None, None)
    };

    let devices = audio_capture::CaptureDevices { mic_name, loopback, live_buffer };
    let handle = audio_capture::start_call_capture_with(&path, devices).map_err(|e| e.to_string())?;
    *state.capture.lock().unwrap() = Some(handle);
    *state.active_recording.lock().unwrap() = Some(rec_id);
    *state.live_worker.lock().unwrap() = live_handle;
    Ok(rec_id)
}

fn resolve_loopback(
    source: &str,
    device_name: Option<String>,
) -> Result<audio_capture::LoopbackSource, String> {
    match source {
        "" | "default" | "device" => Ok(audio_capture::LoopbackSource::Device { name: device_name }),
        #[cfg(target_os = "windows")]
        "discord" => {
            let pid = proc_loopback::find_pid(&["Discord.exe", "DiscordCanary.exe", "DiscordPTB.exe"])
                .ok_or_else(|| "Discord is not running — start the call first".to_string())?;
            Ok(audio_capture::LoopbackSource::Process { pid, label: "Discord".into() })
        }
        #[cfg(target_os = "windows")]
        "teams" => {
            let pid = proc_loopback::find_pid(&["ms-teams.exe", "Teams.exe", "Microsoft.AAD.BrokerPlugin.exe"])
                .ok_or_else(|| "Teams is not running".to_string())?;
            Ok(audio_capture::LoopbackSource::Process { pid, label: "Teams".into() })
        }
        #[cfg(target_os = "windows")]
        s if s.starts_with("pid:") => {
            let pid: u32 = s[4..].parse().map_err(|_| format!("bad pid: {s}"))?;
            Ok(audio_capture::LoopbackSource::Process { pid, label: format!("pid {pid}") })
        }
        other => Err(format!("unknown loopback source: {other}")),
    }
}

#[derive(serde::Serialize)]
pub struct AudioDevices {
    inputs: Vec<String>,
    outputs: Vec<String>,
    default_input: Option<String>,
    default_output: Option<String>,
}

#[tauri::command]
async fn import_recordings(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<Vec<ingest::ImportResult>, String> {
    let results = run_import(&app, paths.into_iter().map(std::path::PathBuf::from).collect()).await;
    Ok(results)
}

#[derive(serde::Serialize)]
pub struct SyncSummary {
    results: Vec<ingest::ImportResult>,
    verified_count: usize,
    failed_count: usize,
    wiped_count: usize,
    wipe_error: Option<String>,
}

#[tauri::command]
async fn sync_device(
    app: tauri::AppHandle,
    mount: String,
    paths: Vec<String>,
    wipe: bool,
) -> Result<SyncSummary, String> {
    let paths_buf: Vec<std::path::PathBuf> =
        paths.into_iter().map(std::path::PathBuf::from).collect();
    let results = run_import(&app, paths_buf).await;

    let verified_sources: Vec<std::path::PathBuf> = results
        .iter()
        .filter(|r| r.verified)
        .map(|r| r.source_path.clone())
        .collect();
    let verified_count = verified_sources.len();
    let failed_count = results.len() - verified_count;

    let (wiped_count, wipe_error) = if wipe && !verified_sources.is_empty() {
        match ingest::wipe_verified(&std::path::PathBuf::from(&mount), &verified_sources) {
            Ok(n) => (n, None),
            Err(e) => (0, Some(e.to_string())),
        }
    } else {
        (0, None)
    };

    Ok(SyncSummary {
        results,
        verified_count,
        failed_count,
        wiped_count,
        wipe_error,
    })
}

async fn run_import(
    app: &tauri::AppHandle,
    paths: Vec<std::path::PathBuf>,
) -> Vec<ingest::ImportResult> {
    let state = app.state::<AppState>();
    let data_dir = state.data_dir.clone();
    let total = paths.len();
    let mut results = Vec::with_capacity(total);

    for (idx, src) in paths.iter().enumerate() {
        let _ = app.emit(
            "import-progress",
            serde_json::json!({
                "index": idx,
                "total": total,
                "source": src.to_string_lossy(),
                "stage": "copying",
            }),
        );
        // ingest::import_file is sync + does its own hashing; run on the
        // blocking pool so we don't stall the Tauri runtime on big files.
        let db_handle = &state.db;
        let data_dir_clone = data_dir.clone();
        let src_clone = src.clone();
        let res = tokio::task::block_in_place(|| {
            ingest::import_file(db_handle, &data_dir_clone, &src_clone)
        });

        let _ = app.emit(
            "import-progress",
            serde_json::json!({
                "index": idx,
                "total": total,
                "source": src.to_string_lossy(),
                "stage": if res.verified { "verified" } else { "failed" },
                "error": res.error,
            }),
        );

        if let (true, Some(rec_id), Some(dest)) = (res.verified, res.recording_id, res.dest_path.clone()) {
            let app_clone = app.clone();
            let data_dir_clone = data_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = run_pipeline(&app_clone, &data_dir_clone, rec_id, &dest).await {
                    tracing::error!("pipeline for import {rec_id} failed: {e:?}");
                    let _ = app_clone.emit("pipeline-error", format!("{rec_id}: {e:?}"));
                }
            });
        }
        results.push(res);
    }
    results
}

#[tauri::command]
fn list_audio_sessions() -> Vec<proc_loopback::AudioSession> {
    #[cfg(target_os = "windows")]
    {
        proc_loopback::list_audio_sessions()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

#[tauri::command]
fn list_audio_devices() -> AudioDevices {
    let (inputs, outputs) = audio_capture::list_devices();
    let (default_input, default_output) = audio_capture::default_device_names();
    AudioDevices {
        inputs,
        outputs,
        default_input,
        default_output,
    }
}

#[tauri::command]
async fn stop_call_capture(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<i64, String> {
    // Stop the live worker first so it doesn't try to push more rows after
    // we delete them in the finalize pass.
    if let Some(h) = state.live_worker.lock().unwrap().take() {
        h.stop();
    }
    let handle = state
        .capture
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "no active capture".to_string())?;
    // Snapshot any async error before stopping so it isn't lost.
    let async_err = handle.async_error.lock().unwrap().clone();
    if let Some(e) = async_err {
        *state.last_capture_error.lock().unwrap() = Some(e);
    } else {
        *state.last_capture_error.lock().unwrap() = None;
    }
    let path = handle.stop().map_err(|e| e.to_string())?;
    let rec_id = state
        .active_recording
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "no active recording id".to_string())?;

    // Mark duration
    let duration = wav_duration_seconds(&path).ok();
    {
        let conn = state.db.lock().unwrap();
        if let Some(d) = duration {
            let _ = conn.execute(
                "UPDATE recordings SET duration_seconds = ?1 WHERE id = ?2",
                rusqlite::params![d, rec_id],
            );
        }
    }

    // Run pipeline in background; emit events as it progresses.
    let app_handle = app.clone();
    let data_dir = state.data_dir.clone();
    tokio::spawn(async move {
        if let Err(e) = run_pipeline(&app_handle, &data_dir, rec_id, &path).await {
            tracing::error!("pipeline failed: {e:?}");
            let _ = app_handle.emit("pipeline-error", format!("{e:?}"));
        }
    });
    Ok(rec_id)
}

fn wav_duration_seconds(path: &std::path::Path) -> anyhow::Result<f64> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let duration = reader.duration() as f64 / spec.sample_rate as f64;
    Ok(duration)
}

fn set_stage(state: &tauri::State<AppState>, s: &str) {
    *state.last_pipeline_stage.lock().unwrap() = Some(s.to_string());
}

async fn run_pipeline(
    app: &tauri::AppHandle,
    data_dir: &std::path::Path,
    rec_id: i64,
    audio_path: &std::path::Path,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    set_stage(&state, "resolving-models");
    let _ = app.emit("pipeline-stage", ("resolving-models", rec_id));
    let cfg = pipeline::resolve_config(&state.db, data_dir).await?;

    set_stage(&state, "transcribing");
    let _ = app.emit("pipeline-stage", ("transcribing", rec_id));
    tracing::info!("pipeline: transcribing recording {rec_id}");
    // run_finalize is sync + CPU-bound. We're on a Tauri async-runtime
    // thread; this stalls that one worker for the duration but doesn't
    // freeze the UI (Tauri uses a multi-thread runtime).
    pipeline::run_finalize(&state.db, rec_id, audio_path, &cfg)?;

    set_stage(&state, "naming");
    let _ = app.emit("pipeline-stage", ("naming", rec_id));
    pipeline::auto_enroll_self(&state.db, rec_id, audio_path, &cfg).ok();

    set_stage(&state, "summarizing");
    let _ = app.emit("pipeline-stage", ("summarizing", rec_id));
    tracing::info!("pipeline: summarizing recording {rec_id}");
    if let Err(e) = pipeline::run_summarize(&state.db, rec_id, &cfg).await {
        // Don't fail the whole pipeline if summarization is unavailable
        // (Ollama not running, model not pulled, etc).
        tracing::warn!("summarization failed for {rec_id}: {e:#}");
        let _ = app.emit("summarize-error", format!("{rec_id}: {e:#}"));
    }

    {
        let conn = state.db.lock().unwrap();
        let _ = db::update_status(&conn, rec_id, "awaiting_naming");
    }
    set_stage(&state, "complete");
    let _ = app.emit("pipeline-stage", ("complete", rec_id));
    tracing::info!("pipeline: complete for recording {rec_id}");
    Ok(())
}

#[tauri::command]
fn get_summary(state: tauri::State<AppState>, recording_id: i64) -> Result<Option<String>, String> {
    let conn = state.db.lock().unwrap();
    conn.query_row(
        "SELECT text FROM summaries WHERE recording_id = ?1",
        rusqlite::params![recording_id],
        |r| r.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.to_string()),
    })
}

#[tauri::command]
fn get_rolling_summary(state: tauri::State<AppState>, recording_id: i64) -> Result<Option<String>, String> {
    let conn = state.db.lock().unwrap();
    conn.query_row(
        "SELECT text FROM rolling_summary WHERE recording_id = ?1",
        rusqlite::params![recording_id],
        |r| r.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.to_string()),
    })
}

#[tauri::command]
fn capture_levels() -> (f32, f32) {
    audio_capture::level_snapshot()
}

#[derive(serde::Serialize)]
pub struct CaptureState {
    /// "idle" | "recording" | "pipeline"
    state: String,
    recording_id: Option<i64>,
    /// Current pipeline stage if any: transcribing / naming / summarizing / complete.
    pipeline_stage: Option<String>,
}

#[tauri::command]
fn get_capture_state(state: tauri::State<AppState>) -> CaptureState {
    let is_recording = state.capture.lock().unwrap().is_some();
    let rec_id = *state.active_recording.lock().unwrap();
    let stage = state.last_pipeline_stage.lock().unwrap().clone();
    let kind = if is_recording {
        "recording"
    } else if rec_id.is_some() && stage.as_deref().map(|s| s != "complete").unwrap_or(false) {
        "pipeline"
    } else {
        "idle"
    };
    CaptureState {
        state: kind.into(),
        recording_id: rec_id,
        pipeline_stage: stage,
    }
}

#[tauri::command]
fn start_monitor_levels(state: tauri::State<AppState>) -> Result<(), String> {
    // No-op if already capturing (real capture is already pushing levels) or
    // already monitoring.
    if state.capture.lock().unwrap().is_some() {
        return Ok(());
    }
    if state.monitor.lock().unwrap().is_some() {
        return Ok(());
    }
    let (mic_name, loop_name) = {
        let conn = state.db.lock().unwrap();
        let mic = db::get_setting(&conn, "device_mic").ok().flatten().filter(|s| !s.is_empty());
        let lp = db::get_setting(&conn, "device_loopback").ok().flatten().filter(|s| !s.is_empty());
        (mic, lp)
    };
    let h = monitor::start_monitor(mic_name.as_deref(), loop_name.as_deref())
        .map_err(|e| e.to_string())?;
    *state.monitor.lock().unwrap() = Some(h);
    Ok(())
}

#[tauri::command]
fn stop_monitor_levels(state: tauri::State<AppState>) -> Result<(), String> {
    *state.monitor.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
fn get_capture_error(state: tauri::State<AppState>) -> Option<String> {
    let cap = state.capture.lock().unwrap();
    if let Some(h) = cap.as_ref() {
        if let Some(err) = h.async_error.lock().unwrap().as_ref() {
            return Some(err.clone());
        }
    }
    state.last_capture_error.lock().unwrap().clone()
}

#[tauri::command]
fn get_log_dir() -> String {
    dirs::data_local_dir()
        .map(|d| d.join("com.notetaker.app"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[tauri::command]
fn detect_call_app() -> String {
    let mut sys = sysinfo::System::new();
    match call_detector::detect_call_app(&mut sys) {
        call_detector::CallApp::Discord => "discord".into(),
        call_detector::CallApp::Teams => "teams".into(),
        call_detector::CallApp::None => "none".into(),
    }
}

#[tauri::command]
fn export_recording(
    state: tauri::State<AppState>,
    recording_id: i64,
    formats: Vec<String>,
) -> Result<Vec<String>, String> {
    let conn = state.db.lock().unwrap();
    export::export(&conn, &state.data_dir, recording_id, &formats).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Result<std::collections::HashMap<String, String>, String> {
    let conn = state.db.lock().unwrap();
    db::get_settings(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_setting(state: tauri::State<AppState>, key: String, value: String) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    db::set_setting(&conn, &key, &value).map_err(|e| e.to_string())
}

#[tauri::command]
fn models_status(state: tauri::State<AppState>) -> Result<ModelsStatus, String> {
    let conn = state.db.lock().unwrap();
    let live = settings_model(&conn, "transcribe_live").unwrap_or_else(|| "small.en".into());
    let finalize = settings_model(&conn, "transcribe_finalize").unwrap_or_else(|| "medium.en".into());
    let live_path = models::whisper_path(&state.data_dir.join("models"), &live);
    let finalize_path = models::whisper_path(&state.data_dir.join("models"), &finalize);
    let (seg, emb) = models::sherpa_paths(&state.data_dir.join("models"));
    Ok(ModelsStatus {
        whisper_live: live.clone(),
        whisper_live_present: live_path.exists(),
        whisper_finalize: finalize.clone(),
        whisper_finalize_present: finalize_path.exists(),
        sherpa_segmentation_present: seg.exists(),
        sherpa_embedding_present: emb.exists(),
    })
}

#[derive(serde::Serialize)]
pub struct ModelsStatus {
    whisper_live: String,
    whisper_live_present: bool,
    whisper_finalize: String,
    whisper_finalize_present: bool,
    sherpa_segmentation_present: bool,
    sherpa_embedding_present: bool,
}

fn settings_model(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    let raw = db::get_setting(conn, key).ok().flatten()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(str::to_string))
}

#[tauri::command]
async fn download_models(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let data_dir = state.data_dir.clone();
    let (live, finalize) = {
        let conn = state.db.lock().unwrap();
        (
            settings_model(&conn, "transcribe_live").unwrap_or_else(|| "small.en".into()),
            settings_model(&conn, "transcribe_finalize").unwrap_or_else(|| "medium.en".into()),
        )
    };
    let models_dir = data_dir.join("models");

    download_one(&app, &models_dir, &live, "whisper-live").await.map_err(|e| e.to_string())?;
    if live != finalize {
        download_one(&app, &models_dir, &finalize, "whisper-finalize")
            .await
            .map_err(|e| e.to_string())?;
    }

    let _ = app.emit(
        "download-progress",
        models::DownloadProgress {
            label: "sherpa".into(),
            bytes: 0,
            total: None,
            done: false,
        },
    );
    let app_for_seg = app.clone();
    let (_seg, _emb) = ensure_sherpa_with_progress(&models_dir, move |p| {
        let _ = app_for_seg.emit("download-progress", p);
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

async fn download_one(
    app: &tauri::AppHandle,
    models_dir: &std::path::Path,
    name: &str,
    label: &str,
) -> anyhow::Result<()> {
    let path = models::whisper_path(models_dir, name);
    let url = models::whisper_url(name);
    let app2 = app.clone();
    let label_owned = format!("{label}:{name}");
    models::download_with_progress(&url, &path, &label_owned, move |p| {
        let _ = app2.emit("download-progress", p);
    })
    .await
}

async fn ensure_sherpa_with_progress<F: Fn(models::DownloadProgress) + Send + Clone + 'static>(
    models_dir: &std::path::Path,
    cb: F,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let (seg, emb) = models::sherpa_paths(models_dir);
    if !emb.exists() {
        models::download_with_progress(models::EMBEDDING_URL, &emb, "sherpa-embedding", cb.clone()).await?;
    }
    if !seg.exists() {
        let tarball = models_dir.join("sherpa").join("sherpa-onnx-pyannote-segmentation-3-0.tar.bz2");
        models::download_with_progress(models::SEGMENTATION_TARBALL_URL, &tarball, "sherpa-segmentation", cb).await?;
        models::extract_tar_bz2_pub(&tarball, &models_dir.join("sherpa"))?;
        let _ = std::fs::remove_file(&tarball);
    }
    Ok((seg, emb))
}

pub fn run() {
    // Log path is whatever Tauri picks for app_data_dir — we'd love to use it
    // here, but app_data_dir() needs the app handle. So write to a stable
    // location (LocalAppData) and let the app print the resolved path on first
    // launch.
    let log_dir = dirs::data_local_dir()
        .map(|d| d.join("com.notetaker.app"))
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "notetaker.log");
    let (nb_writer, _guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so it lives for the program lifetime.
    Box::leak(Box::new(_guard));

    use tracing_subscriber::EnvFilter;
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("notetaker=debug,notetaker_lib=debug,info"));
    let _ = tracing_subscriber::fmt()
        .with_writer(nb_writer)
        .with_env_filter(env_filter)
        .with_ansi(false)
        .try_init();
    tracing::info!(?log_dir, "notetaker starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            std::fs::create_dir_all(data_dir.join("audio")).ok();
            std::fs::create_dir_all(data_dir.join("models")).ok();
            std::fs::create_dir_all(data_dir.join("exports")).ok();
            let conn = db::open(&data_dir.join("notetaker.db"))?;
            app.manage(AppState {
                db: Mutex::new(conn),
                data_dir,
                capture: Mutex::new(None),
                active_recording: Mutex::new(None),
                hardware_cache: Mutex::new(None),
                last_capture_error: Mutex::new(None),
                live_worker: Mutex::new(None),
                monitor: Mutex::new(None),
                last_pipeline_stage: Mutex::new(None),
            });

            // Start the USB watcher — every plug-in event becomes a
            // `device-plugged` event the UI listens for.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = device_watcher::start_watch();
                while let Some(dev) = rx.recv().await {
                    let _ = handle.emit("device-plugged", &dev);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_hardware,
            redetect_hardware,
            list_recordings,
            get_segments,
            get_unnamed_clusters,
            confirm_speaker,
            start_call_capture,
            stop_call_capture,
            export_recording,
            get_settings,
            set_setting,
            models_status,
            download_models,
            capture_levels,
            start_monitor_levels,
            stop_monitor_levels,
            get_capture_state,
            get_capture_error,
            get_log_dir,
            get_summary,
            get_rolling_summary,
            detect_call_app,
            list_audio_devices,
            list_audio_sessions,
            import_recordings,
            sync_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
