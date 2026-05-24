import { invoke } from "@tauri-apps/api/core";

export type GpuVendor = "Nvidia" | "Intel" | "Amd" | "Unknown";
export type Backend = "Cpu" | "Cuda" | "DirectML";

export interface GpuInfo {
  name: string;
  vendor: GpuVendor;
  vram_mb: number | null;
  cuda_capable: boolean;
  directml_capable: boolean;
}

export interface HardwareProfile {
  cpu_cores: number;
  total_ram_gb: number;
  gpus: GpuInfo[];
  recommended_backend: Backend;
  recommended_whisper_live: string;
  recommended_whisper_finalize: string;
  recommended_llm: string;
}

export interface RecordingRow {
  id: number;
  source_filename: string | null;
  source: "device" | "call";
  app_name: string | null;
  imported_at: string;
  duration_seconds: number | null;
  status: string;
}

export interface SegmentRow {
  id: number;
  speaker_id: number | null;
  speaker_name: string | null;
  start_seconds: number;
  end_seconds: number;
  text: string;
  confidence: number | null;
}

export interface UnnamedCluster {
  cluster_id: number;
  snippets: { start: number; end: number; text: string; audio_b64?: string }[];
  suggestions: { speaker_id: number; name: string; similarity: number }[];
}

export interface ModelsStatus {
  whisper_live: string;
  whisper_live_present: boolean;
  whisper_finalize: string;
  whisper_finalize_present: boolean;
  sherpa_segmentation_present: boolean;
  sherpa_embedding_present: boolean;
}

export interface DownloadProgress {
  label: string;
  bytes: number;
  total: number | null;
  done: boolean;
}

export const api = {
  modelsStatus: () => invoke<ModelsStatus>("models_status"),
  downloadModels: () => invoke<void>("download_models"),
  detectHardware: () => invoke<HardwareProfile>("detect_hardware"),
  reDetectHardware: () => invoke<HardwareProfile>("redetect_hardware"),
  listRecordings: () => invoke<RecordingRow[]>("list_recordings"),
  getSegments: (recordingId: number) =>
    invoke<SegmentRow[]>("get_segments", { recordingId }),
  getUnnamedClusters: (recordingId: number) =>
    invoke<UnnamedCluster[]>("get_unnamed_clusters", { recordingId }),
  confirmSpeaker: (
    recordingId: number,
    clusterId: number,
    decision:
      | { kind: "new"; name: string }
      | { kind: "existing"; speakerId: number }
      | { kind: "merge"; intoClusterId: number }
      | { kind: "noise" },
  ) => invoke<void>("confirm_speaker", { recordingId, clusterId, decision }),
  startCallCapture: () => invoke<number>("start_call_capture"),
  stopCallCapture: () => invoke<void>("stop_call_capture"),
  exportRecording: (recordingId: number, formats: ("vtt" | "json" | "md")[]) =>
    invoke<string[]>("export_recording", { recordingId, formats }),
  getSettings: () => invoke<Record<string, string>>("get_settings"),
  setSetting: (key: string, value: string) =>
    invoke<void>("set_setting", { key, value }),
  captureLevels: () => invoke<[number, number]>("capture_levels"),
  startMonitorLevels: () => invoke<void>("start_monitor_levels"),
  stopMonitorLevels: () => invoke<void>("stop_monitor_levels"),
  getCaptureState: () =>
    invoke<{
      state: "idle" | "recording" | "pipeline";
      recording_id: number | null;
      pipeline_stage: string | null;
    }>("get_capture_state"),
  getCaptureError: () => invoke<string | null>("get_capture_error"),
  logDir: () => invoke<string>("get_log_dir"),
  getSummary: (recordingId: number) =>
    invoke<string | null>("get_summary", { recordingId }),
  getRollingSummary: (recordingId: number) =>
    invoke<string | null>("get_rolling_summary", { recordingId }),
  detectCallApp: () => invoke<"discord" | "teams" | "none">("detect_call_app"),
  listAudioDevices: () =>
    invoke<{
      inputs: string[];
      outputs: string[];
      default_input: string | null;
      default_output: string | null;
    }>("list_audio_devices"),
  listAudioSessions: () =>
    invoke<{ pid: number; display_name: string; process_name: string }[]>(
      "list_audio_sessions",
    ),
  importRecordings: (paths: string[]) =>
    invoke<ImportResult[]>("import_recordings", { paths }),
  syncDevice: (mount: string, paths: string[], wipe: boolean) =>
    invoke<SyncSummary>("sync_device", { mount, paths, wipe }),
};

export interface ImportResult {
  source_path: string;
  recording_id: number | null;
  dest_path: string | null;
  bytes: number;
  source_sha256: string;
  dest_sha256: string;
  verified: boolean;
  error: string | null;
}

export interface SyncSummary {
  results: ImportResult[];
  verified_count: number;
  failed_count: number;
  wiped_count: number;
  wipe_error: string | null;
}

export interface ImportProgress {
  index: number;
  total: number;
  source: string;
  stage: "copying" | "verified" | "failed";
  error?: string | null;
}

export interface PluggedDevice {
  mount: string;
  label: string | null;
  audio_files: string[];
}

// Cache the hardware profile in memory so navigating to the page repeatedly
// doesn't re-run nvidia-smi / WMI.
let _hwCache: HardwareProfile | null = null;
export async function getHardwareCached(): Promise<HardwareProfile> {
  if (_hwCache) return _hwCache;
  _hwCache = await api.detectHardware();
  return _hwCache;
}
export async function reDetectHardware(): Promise<HardwareProfile> {
  _hwCache = await api.reDetectHardware();
  return _hwCache;
}
