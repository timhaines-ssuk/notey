# Implementation status

Companion to [PROJECT_PLAN.md](./PROJECT_PLAN.md). The plan is the canonical
design; this file is the running diff between the plan and what's actually in
the repo, plus the decisions taken along the way and the work still pending.

Last updated: 2026-05-24 (commit at the time of writing: see `git log`).

---

## 1. Where the original plan ended up

Everything in PROJECT_PLAN.md §2 (stack) is in use except CUDA, which is opt-in
rather than default — see decision #1 below.

| Plan section | Status | File |
|---|---|---|
| §3 Hardware detection + recommendation matrix | done | [hardware.rs](src-tauri/src/hardware.rs), [bin/spike_hwdetect.rs](src-tauri/src/bin/spike_hwdetect.rs) |
| §4 Capture (WASAPI loopback + mic, stereo WAV) | done | [audio_capture.rs](src-tauri/src/audio_capture.rs) |
| §4 Transcribe (live + finalize) | done | [transcribe.rs](src-tauri/src/transcribe.rs), [live.rs](src-tauri/src/live.rs) |
| §4 Diarize + identify | done | [diarize.rs](src-tauri/src/diarize.rs) |
| §4/§5 Speaker naming UI | done | [speakers.rs](src-tauri/src/speakers.rs), [SpeakerNamingDialog.tsx](src/components/SpeakerNamingDialog.tsx) |
| §4 Summarize | partial — code exists, pipeline doesn't call it yet | [summarize.rs](src-tauri/src/summarize.rs) |
| §4 Export (VTT/JSON/MD) | done | [export.rs](src-tauri/src/export.rs) |
| §4 Cleanup (delete audio post-finalize) | done | [pipeline.rs:`finalize_cleanup`](src-tauri/src/pipeline.rs) |
| §6 Database schema | done | [db.rs](src-tauri/src/db.rs) |
| §7 VTT format (Teams `<v Speaker>` cues, NOTE header) | done | [export.rs:`write_vtt`](src-tauri/src/export.rs) |
| §8 Project layout | done | (see tree) |
| §9 Installation guide | mostly done — LLVM, Ollama, MSVC/CMake, Node all installed | [README.md](README.md) |
| §10 Cargo dependencies | mostly aligned; see decision #2 | [src-tauri/Cargo.toml](src-tauri/Cargo.toml) |
| §11 Spikes #1, #2, #3 | written; #3 runs end-to-end on real hardware | [src-tauri/src/bin/](src-tauri/src/bin/) |
| §11 Spike #4 manual cluster browser | not built — deferred until first real recording | — |
| §13 NekSide arrival checklist | pending — recorder hasn't arrived | — |

---

## 2. Significant decisions taken since the plan

### #1 — CUDA is opt-in, not default

Plan §10 had `default = ["cuda"]`. We dropped that: the default feature set is
empty, `--features ml` enables CPU whisper+sherpa, `--features cuda` enables
CUDA on top. Reason: the CUDA Toolkit (~3 GB installer) isn't installed yet,
and we wanted shippable artifacts before paying that download. Once CUDA is
installed, the existing feature flag in [Cargo.toml](src-tauri/Cargo.toml)
produces a CUDA build with no further changes.

### #2 — Dependency version pins changed

- **whisper-rs 0.13 → 0.16**: 0.13 was incompatible with clang 22 (bindgen
  produced `_address`-only struct definitions). 0.16 also has a new API:
  `state.full_n_segments()` returns `c_int` directly, segments are accessed
  via `state.get_segment(i)` → `WhisperSegment { start_timestamp, end_timestamp, to_str }`,
  no more `full_get_segment_t0/t1/text`.
- **sherpa-rs `0.7` → `0.6.8`**: 0.7 doesn't exist on crates.io. The 0.6 API
  is also different from what the plan assumed — the type is `Diarize` not
  `SpeakerDiarization`, config is `DiarizeConfig`, `compute()` takes
  `Vec<f32>` + optional progress callback, returns
  `Vec<Segment { start, end, speaker }>` with no per-segment embeddings.
  We pool embeddings ourselves via `EmbeddingExtractor::compute_speaker_embedding`
  in [diarize.rs:`extract_cluster_embeddings`](src-tauri/src/diarize.rs).

### #3 — Per-process WASAPI loopback (OBS-style) added

Not in the original plan. After the first Discord-capture conversation, we
added [proc_loopback.rs](src-tauri/src/proc_loopback.rs) using
`ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_PARAMS` with
`PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` — same mechanism OBS's
"Application Audio Capture" source uses. The capture pipeline now supports
either device-level loopback (cpal) or per-process loopback as the
right-channel source, selectable in **Settings → Audio**.

Also added: WASAPI session enumeration
([proc_loopback.rs:`enumerate_audio_sessions`](src-tauri/src/proc_loopback.rs))
to populate the picker with *all* currently-running audio processes, not just
hardcoded Discord/Teams.

### #4 — Audio device picker

Plan implicitly assumed OS defaults. Added a UI for picking the mic input and
the loopback output device explicitly
([SettingsAudio.tsx](src/components/SettingsAudio.tsx)), persisted in the
`settings` table as `device_mic` / `device_loopback` / `loopback_source`.

### #5 — Mid-recording RMS meters

Live per-channel RMS atomics
([audio_capture.rs:`LAST_MIC_RMS_X1000`](src-tauri/src/audio_capture.rs))
exposed via `capture_levels` command and shown as bars on the Recordings page
during capture. Lets the user confirm both channels are actually receiving
audio before trusting the recording. Not in the plan; added for Discord
debugging.

### #6 — Hardware detection cached at two layers

Plan §3.1 says cache the detection result; we cache it twice — in
`AppState.hardware_cache` (Rust) and in a module-level let in
[tauri-api.ts:`getHardwareCached`](src/lib/tauri-api.ts) (TS). The Rust cache
survives across page nav within a session; the TS cache also survives without
even crossing the Tauri bridge. "Re-detect hardware" busts both.

### #7 — SHA-256 verified USB sync, atomic rename, safe wipe

Plan §4 said "delete audio after processing confirmed successful" but didn't
specify what "confirmed" means. We implemented in
[ingest.rs:`try_import_file`](src-tauri/src/ingest.rs):

1. Stat source for expected size.
2. Stream-copy to `dest.partial`, computing SHA-256 over the bytes as they're read.
3. `sync_all()` to force flush to disk.
4. Independently SHA-256 the on-disk file.
5. Source SHA == dest SHA *and* dest size == expected → atomic rename to final
   path and update DB row to `transcribing`. Mismatch → delete partial, mark
   row `failed`.

Then in [ingest.rs:`wipe_verified`](src-tauri/src/ingest.rs): only deletes
sources whose copy passed verification, and only if `canonicalize(path)`
starts with `canonicalize(mount)` (guards against symlink shenanigans).

### #8 — Live rolling-transcription windowing

Plan §4 said "rolling buffer" without specifying. Implemented in
[live.rs](src-tauri/src/live.rs) as 8-second windows with 2-second overlap
between them, deduplicated by `last_emit_end`. The finalize pass deletes the
live rows and re-inserts authoritative ones from the larger model
([pipeline.rs:`run_finalize`](src-tauri/src/pipeline.rs) — note the explicit
`DELETE FROM segments WHERE recording_id = ?` and matching clusters delete).

### #9 — File logging via tracing-appender

Plan §10 had `tracing-subscriber = "0.3"` only. The release exe has no
console, so tracing output went nowhere. Added `tracing-appender` writing
daily-rolling logs to `%LOCALAPPDATA%\com.notetaker.app\notetaker.log.YYYY-MM-DD`
([lib.rs:`run`](src-tauri/src/lib.rs)). Path is also exposed to the UI via
the `get_log_dir` command + a copy-to-clipboard button in Settings → Audio.

### #10 — Tarball extraction via Windows built-in `tar`

Plan §9.3 gave a `.tar.bz2` URL for the sherpa segmentation model but no
plan for extraction. We shell out to `tar -xjf` (the BSD tar that ships with
Windows 10 1803+/11) in [models.rs:`extract_tar_bz2`](src-tauri/src/models.rs)
rather than pulling in a `bzip2` + `tar` crate combo for one use.

### #11 — Mic auto-enroll via channel energy, not "always-left"

Plan §5 said the mic channel is always the user, implying we'd
unconditionally bind cluster=0 to Self. Diarization output cluster IDs are
arbitrary and don't correlate with channel layout. Instead, in
[pipeline.rs:`auto_enroll_self`](src-tauri/src/pipeline.rs) we measure RMS
energy of the mic channel for each cluster's time ranges and pick the cluster
with the highest mean — that's the one whose segments contain the most of
the user's voice.

### #12 — v1 USB sync requires WAV, not arbitrary audio

Plan §4 implied any audio format. The importer currently rejects anything
other than `.wav` with a message pointing to `ffmpeg -i in.mp3 out.wav`. We
held off on adding symphonia (pure-Rust audio decode) until the actual
NekSide arrives and we know what format(s) it produces.

### #13 — Hardware-detection re-run only on explicit user action

Two commands now: `detect_hardware` (cache-aware) and `redetect_hardware`
(forces re-scan and updates the cache). The Settings → Hardware page calls
the cached one on mount; the **Re-detect hardware** button is the only path
that fires `nvidia-smi` / WMI a second time.

---

## 3. Done but not yet plumbed end-to-end

These have code written but aren't fully wired to the user flow:

- **Live rolling transcription** ([live.rs](src-tauri/src/live.rs)): module exists,
  `LiveBuffer` works, `spawn_live` is callable, but neither
  [audio_capture.rs](src-tauri/src/audio_capture.rs) feeds the capture buffer
  into a `LiveBuffer` nor does [lib.rs](src-tauri/src/lib.rs) start a live
  worker on `start_call_capture`. The finalize-only path runs on stop. To
  enable live, hook the cpal/proc_loopback frame callbacks to also push into
  a `LiveBuffer`, and emit `live-segment` Tauri events the frontend already
  declares but doesn't yet display.
- **Summarization** ([summarize.rs](src-tauri/src/summarize.rs)): rolling-chunk
  grouping, Ollama HTTP client, prompts all implemented; but
  [pipeline.rs:`run_finalize`](src-tauri/src/pipeline.rs) doesn't call any of
  it after diarization finishes. `summary_chunks`, `rolling_summary`, and
  `summaries` tables stay empty. Wire-up is: after status moves to
  `awaiting_naming` (or after speaker naming, depending on whether you want
  named summaries), iterate `group_into_chunks(segments)`, call
  `ollama_generate(chunk_prompt(...))` per chunk, then `final_prompt(...)`
  once.
- **Exports history** (`exports` table): rows are written on every export
  but the UI doesn't list previous exports — would be a one-screen addition.
- **First-run hardware-recommended model preselection**: the recommendation
  matrix runs and the user sees it on Settings → Hardware, but the model
  downloader uses the seeded defaults (`small.en` / `medium.en` /
  `qwen2.5:7b`) regardless. Should pre-populate the `transcribe_live` /
  `transcribe_finalize` / `summarize` settings from the recommendation on
  first run, then let the user override.

---

## 4. Plan items not yet implemented

In rough priority order:

1. **Spike #4 — manual cluster browser**. Plan §11 step 4: after diarization,
   dump each cluster's segments as `clusters/SPEAKER_XX/*.wav` and listen by
   ear. Needed once we have a real recording so we can calibrate the §5
   snippet-picking criteria.
2. **§13 NekSide checklist**. The recorder hasn't arrived. Most of §13
   needs the physical device.
3. **Channel-split diarization first pass for dual-channel calls**. Plan §4
   diarize step says "For dual-channel calls: channel separation does first
   pass for free". Right now diarization runs on the mono downmix. The mic
   auto-enroll covers the "you vs everyone else" split, but for a 3+ person
   Discord call diarization still has to distinguish remote speakers from
   each other on the mixed right channel — which is the hard case. The fix
   is to diarize each channel separately and merge, but it doubles
   diarization cost.
4. **Embedding threshold calibration** (plan §12 risk #5). The 0.55 / 0.75
   thresholds in [speakers.rs](src-tauri/src/speakers.rs) are placeholders;
   we need to plot cosine-similarity distributions on real recordings before
   trusting them.
5. **CUDA-enabled release build**. Toolchain installed except for CUDA. The
   `--features cuda` flag is wired and will produce a CUDA build as soon as
   the CUDA Toolkit 12.6 is installed.
6. **mp3 / m4a / flac import** (plan §13 risk #1: NekSide format unknown
   until hardware arrives). Add `symphonia` decode → 16 kHz mono WAV in
   [ingest.rs](src-tauri/src/ingest.rs).
7. **VAD-based summary-chunk splitting** (plan §4: "prefer VAD silence
   boundaries"). [summarize.rs:`group_into_chunks`](src-tauri/src/summarize.rs)
   uses a simple "any gap > 1s ends a chunk" heuristic; the plan calls for
   actual VAD via sherpa's Silero or Ten VAD. Quality difference is unclear
   without test data.
8. **Auto-start capture on Discord/Teams launch**. Currently the chip shows
   "Discord detected" but the user clicks Start. Plan §4 had it implicit but
   never specified auto-trigger.
9. **Bluetooth HFP audio handling** (plan §12 risk #4). No mitigation in
   place. Probably a "warn the user, link to docs" feature rather than a
   technical fix.
10. **Drag-and-drop import**. Right now the only manual-import path is the
    Tauri file picker; HTML5 drag-and-drop would be nicer.

---

## 5. Known issues / risks observed

- **Discord process-loopback returning silence (under investigation).** Symptom
  reported after the first live test. v8 release adds error surfacing, capture
  meters, and file logging to triage this. Possible causes: clash with OBS's
  Application Audio Capture (officially supported to coexist, but worth A/B
  testing); Discord targeting a non-default render device; activation
  succeeding but the engine returning `AUDCLNT_BUFFERFLAGS_SILENT` packets.
- **`pipeline.rs:run_finalize` is blocking-sync but called from a `tokio::spawn`.**
  This currently works because `block_in_place` is used in `run_import` for
  the import path, and the call-capture path doesn't yet do anything blocking
  before the spawn. If we add CPU-heavy work earlier in the pipeline we'll
  need to be more careful.
- **`auto_enroll_self` runs even if speaker naming has already been done.** If
  the user already named the mic-channel speaker (e.g. via the dialog) and
  then re-runs the pipeline, we'd overwrite that with "You". Need a guard
  on "speakers.is_self IS NULL" before re-binding.
- **Build artifacts include placeholder icons.** The `src-tauri/icons/*.png`
  and `icon.ico` are auto-generated blue "N" placeholders, not the eventual
  branded assets.
