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
fn start_call_capture(state: tauri::State<AppState>) -> Result<i64, String> {
    let conn = state.db.lock().unwrap();
    let mic_name = db::get_setting(&conn, "device_mic").ok().flatten().filter(|s| !s.is_empty());
    let loop_source = db::get_setting(&conn, "loopback_source").ok().flatten().unwrap_or_default();
    let loop_name = db::get_setting(&conn, "device_loopback").ok().flatten().filter(|s| !s.is_empty());
    let rec_id = db::create_call_recording(&conn, &state.data_dir).map_err(|e| e.to_string())?;
    drop(conn);

    let loopback = resolve_loopback(&loop_source, loop_name)?;

    let path = state.data_dir.join("audio").join(format!("call_{rec_id}.wav"));
    let devices = audio_capture::CaptureDevices { mic_name, loopback };
    let handle = audio_capture::start_call_capture_with(&path, devices).map_err(|e| e.to_string())?;
    *state.capture.lock().unwrap() = Some(handle);
    *state.active_recording.lock().unwrap() = Some(rec_id);
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
    let handle = state
        .capture
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "no active capture".to_string())?;
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

async fn run_pipeline(
    app: &tauri::AppHandle,
    data_dir: &std::path::Path,
    rec_id: i64,
    audio_path: &std::path::Path,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let cfg = pipeline::resolve_config(&state.db, data_dir).await?;

    let _ = app.emit("pipeline-stage", ("transcribing", rec_id));
    pipeline::run_finalize(&state.db, rec_id, audio_path, &cfg)?;
    let _ = app.emit("pipeline-stage", ("naming", rec_id));

    // Mic auto-enroll: channel 0 (left = mic) is always "You". The mic cluster
    // is the one whose segments fall predominantly in channel 0; for stereo
    // mic-on-L / loopback-on-R recordings we detect that by checking the
    // mic-channel transcript pass.
    pipeline::auto_enroll_self(&state.db, rec_id, audio_path, &cfg).ok();

    let _ = app.emit("pipeline-stage", ("complete", rec_id));
    Ok(())
}

#[tauri::command]
fn capture_levels() -> (f32, f32) {
    audio_capture::level_snapshot()
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
    tracing_subscriber::fmt::init();

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
            detect_call_app,
            list_audio_devices,
            list_audio_sessions,
            import_recordings,
            sync_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
