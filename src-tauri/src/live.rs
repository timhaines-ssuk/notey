//! Live (rolling) transcription. Reads samples from a shared buffer fed by
//! the capture stream, and every `LIVE_WINDOW_SECONDS` seconds runs the
//! "live" whisper model over the last `LIVE_OVERLAP_SECONDS + window` of
//! audio. The output is emitted to the frontend as `live-segment` events
//! and inserted into the DB with status='live' so the user sees text appear
//! as the call progresses.
//!
//! The final pass (after stop) re-transcribes the full WAV with the bigger
//! "finalize" model and *replaces* these rows.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LIVE_WINDOW_SECONDS: u64 = 8;
pub const LIVE_OVERLAP_SECONDS: u64 = 2;

#[derive(Clone)]
pub struct LiveBuffer {
    inner: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
}

impl LiveBuffer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            sample_rate,
        }
    }
    pub fn push(&self, samples: &[f32]) {
        self.inner.lock().unwrap().extend_from_slice(samples);
    }
    pub fn take_recent(&self, seconds: u64) -> Vec<f32> {
        let buf = self.inner.lock().unwrap();
        let want = (seconds as usize) * self.sample_rate as usize;
        let start = buf.len().saturating_sub(want);
        buf[start..].to_vec()
    }
    pub fn len_samples(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

pub struct LiveHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LiveHandle {
    pub fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn spawn_live<F: Fn(crate::transcribe::TranscribedSegment) + Send + 'static>(
    buffer: LiveBuffer,
    model_path: PathBuf,
    device_index: i32,
    on_segment: F,
) -> LiveHandle {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut last_emit_end: f64 = 0.0;
        loop {
            // Poll the stop flag at 100 ms intervals instead of sleeping for
            // the full window duration. Worst-case stop latency: 100 ms here
            // (vs ~8 s previously) + whatever the in-flight whisper call
            // takes, which we also skip if stop fires before we enter it.
            let deadline = Instant::now()
                + Duration::from_secs(LIVE_WINDOW_SECONDS);
            while Instant::now() < deadline {
                if stop_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if stop_thread.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }

            let total_samples = buffer.len_samples();
            let mut offset_seconds = total_samples as f64 / buffer.sample_rate as f64
                - (LIVE_WINDOW_SECONDS + LIVE_OVERLAP_SECONDS) as f64;
            if offset_seconds < 0.0 {
                offset_seconds = 0.0;
            }
            let chunk = buffer.take_recent(LIVE_WINDOW_SECONDS + LIVE_OVERLAP_SECONDS);
            if chunk.is_empty() {
                continue;
            }

            // Final stop check just before the expensive whisper call. If
            // the user hits Stop near a window boundary, this saves the 1-5 s
            // whisper inference time on the way out.
            if stop_thread.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            match transcribe_chunk(&model_path, &chunk, device_index) {
                Ok(segments) => {
                    for mut s in segments {
                        let absolute_start = s.start_seconds + offset_seconds;
                        let absolute_end = s.end_seconds + offset_seconds;
                        if absolute_end <= last_emit_end {
                            continue;
                        }
                        s.start_seconds = absolute_start;
                        s.end_seconds = absolute_end;
                        last_emit_end = absolute_end;
                        on_segment(s);
                    }
                }
                Err(e) => {
                    tracing::warn!("live transcribe failed: {e:?}");
                }
            }
        }
    });
    LiveHandle {
        stop,
        thread: Some(thread),
    }
}

#[cfg(feature = "ml")]
fn transcribe_chunk(
    model_path: &std::path::Path,
    samples: &[f32],
    device_index: i32,
) -> Result<Vec<crate::transcribe::TranscribedSegment>> {
    crate::transcribe::transcribe_samples(
        model_path,
        samples,
        crate::transcribe::Mode::Live,
        device_index,
    )
}

#[cfg(not(feature = "ml"))]
fn transcribe_chunk(
    _model_path: &std::path::Path,
    _samples: &[f32],
    _device_index: i32,
) -> Result<Vec<crate::transcribe::TranscribedSegment>> {
    Ok(Vec::new())
}
