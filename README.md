# Notetaker

Local AI notetaker — Tauri 2 + React + Rust. See [PROJECT_PLAN.md](./PROJECT_PLAN.md) for the design (this scaffold implements that plan).

## Toolchain prerequisites

These have to be installed once per machine via GUI installers — they can't be vendored or installed by `cargo`/`npm`. See PROJECT_PLAN.md §9.1 for the canonical list with download links.

Required for the bare app to build (Rust side):
- **MSVC Build Tools** (Visual Studio 2022, "Desktop development with C++" workload). After install, `cl` must be on PATH.
- **CMake** (bundled with the workload above, but make sure `cmake --version` works).

Required additionally for the ML features (`--features ml` or `--features cuda`):
- **LLVM/Clang** ≥ 16, with `LIBCLANG_PATH` set to e.g. `C:\Program Files\LLVM\bin`.
- **CUDA Toolkit 12.6** (only for `--features cuda`).

Always needed at runtime:
- **Ollama** running on `http://localhost:11434` (for summarisation).
- The whisper/sherpa-onnx model files (downloaded automatically by the app on first run).

You already have: Rust 1.94, Node 24, and a CUDA-capable RTX 4060 Ti (8 GB).

## Running

```powershell
npm install
cargo install tauri-cli --version "^2.0"
# bare scaffold (UI + DB + hardware detection, no transcription):
cargo tauri dev
# with transcription + diarisation:
cargo tauri dev -- --features cuda
```

## Spikes (PROJECT_PLAN.md §11)

Each spike is a standalone CLI in `src-tauri/src/bin/` so it can be run before the full app is wired up. Run from `src-tauri/`:

```powershell
cargo run --bin spike-hwdetect                               # §11 #3
cargo run --bin spike-capture -- out.wav 30                  # §11 #2 — 30s WASAPI loopback+mic
cargo run --bin spike-asr --features cuda -- in.wav model.bin   # §11 #1
```

## Layout

```
notetaker/
├── PROJECT_PLAN.md      design + decisions
├── src/                 React frontend
├── src-tauri/
│   └── src/
│       ├── lib.rs           Tauri commands + AppState
│       ├── main.rs          entry
│       ├── hardware.rs      §3 GPU/CPU detection + recommendation matrix
│       ├── models.rs        whisper/sherpa registry + downloader
│       ├── db.rs            SQLite schema (§6) + helpers
│       ├── audio_capture.rs WASAPI loopback + mic → stereo WAV
│       ├── transcribe.rs    whisper-rs wrapper (feature-gated)
│       ├── diarize.rs       sherpa-rs wrapper + speaker math
│       ├── speakers.rs      naming UI backend (§5)
│       ├── summarize.rs     Ollama HTTP + rolling chunk logic
│       ├── export.rs        VTT/JSON/MD (§7)
│       ├── device_watcher.rs USB plug-in poller
│       ├── call_detector.rs Discord/Teams process check
│       ├── pipeline.rs      orchestrator
│       └── bin/             spike CLIs
```

## First test — live Discord call

1. Make sure Windows' default playback device is the one Discord is actually using (Settings → System → Sound → Output). The app captures system audio via WASAPI loopback on the *default* device — if Discord is routed elsewhere, the loopback channel will be silent.
2. Make sure your mic is the default input device.
3. Launch the app (`cargo tauri dev --features ml` for now; release exe after build completes).
4. On first run the model downloader appears — download whisper + sherpa models (~2 GB). They cache to `%APPDATA%\com.notetaker.app\models\`.
5. Go to **Recordings**. If Discord is running you'll see a blue "Discord detected" chip.
6. Join your voice channel, then click **Start call capture**.
7. Watch the two meters mid-call: top is your mic, bottom is system audio. **Both should move.** If the system-audio meter is flat, Discord is on a non-default output device — fix it and restart capture.
8. Talk for a few minutes. Stop capture; the pipeline runs in the background (transcribe → diarize → mic auto-enroll as "You" → status `awaiting_naming`).
9. Open the recording, click **Name speakers** to label remote voices, then **Export VTT**.

## Status

- [x] Frontend scaffold (Vite/React/TS, all five screens)
- [x] Rust crate scaffold + Cargo deps
- [x] DB schema + queries
- [x] Hardware detection + recommendation matrix
- [x] Model registry + HF downloader
- [x] Audio capture (cpal WASAPI loopback + mic, 16 kHz stereo WAV)
- [x] Whisper wrapper (feature-gated)
- [x] Sherpa diarization wrapper + cosine match / running-mean merge
- [x] Speaker naming dialog + DB-side `confirm` logic
- [x] Ollama summarisation client + chunking
- [x] VTT/JSON/MD exporters (Teams-compatible `<v Speaker>` cues)
- [x] USB device watcher (drive-letter poller)
- [x] Call detector (Discord/Teams process check)
- [x] Pipeline orchestrator + Tauri commands
- [x] First-run model download UI flow
- [x] Live (rolling) transcription path (rolling 8s windows with 2s overlap)
- [x] Mic-channel auto-enrolment as "Self"
- [x] Live call detection (Discord/Teams process check) — UI chip
- [x] Capture meter (mic + loopback) so you can verify both channels mid-recording
- [ ] Spike #4 (manual cluster browser) and §13 NekSide validation — needs hardware
```
