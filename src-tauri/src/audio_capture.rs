//! Stereo WAV capture: mic on left, WASAPI loopback (system audio) on right.
//!
//! Both streams are resampled to a common 16 kHz mono float-per-channel stream
//! (Whisper-friendly) and interleaved before being written to disk by `hound`.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig};
use hound::{SampleFormat as WavFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Last-known per-channel RMS (×1000, as u64 for atomic). Exposed so the UI
/// can show a meter mid-recording — useful for confirming Discord loopback is
/// actually feeding audio before you trust the recording.
pub static LAST_MIC_RMS_X1000: AtomicU64 = AtomicU64::new(0);
pub static LAST_LOOPBACK_RMS_X1000: AtomicU64 = AtomicU64::new(0);

pub fn level_snapshot() -> (f32, f32) {
    let m = LAST_MIC_RMS_X1000.load(Ordering::Relaxed) as f32 / 1000.0;
    let l = LAST_LOOPBACK_RMS_X1000.load(Ordering::Relaxed) as f32 / 1000.0;
    (m, l)
}

const TARGET_SR: u32 = 16_000;

pub struct CaptureHandle {
    stop_tx: Sender<()>,
    join: Option<JoinHandle<Result<PathBuf>>>,
    /// Set by the capture thread if process-loopback (or anything else)
    /// failed AFTER the initial start_call_capture() returned. The UI polls
    /// this via the `get_capture_error` command so the user finds out before
    /// they hit Stop.
    pub async_error: Arc<Mutex<Option<String>>>,
}

impl CaptureHandle {
    pub fn stop(mut self) -> Result<PathBuf> {
        let _ = self.stop_tx.send(());
        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("capture already joined"))?;
        join.join().map_err(|_| anyhow!("capture thread panicked"))?
    }
}

#[derive(Debug, Clone)]
pub enum LoopbackSource {
    /// WASAPI loopback on a named output device (or OS default if None).
    Device { name: Option<String> },
    /// Per-process loopback (Windows 10 20348+ / Windows 11) on the given PID,
    /// including its full process tree. Use `proc_loopback::find_pid(...)` to
    /// resolve a name like `discord.exe`.
    Process { pid: u32, label: String },
}

impl Default for LoopbackSource {
    fn default() -> Self {
        Self::Device { name: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CaptureDevices {
    /// Name of the input device (mic) to capture from. `None` → OS default.
    pub mic_name: Option<String>,
    /// Where the right (loopback) channel comes from.
    pub loopback: LoopbackSource,
}

pub fn list_devices() -> (Vec<String>, Vec<String>) {
    use cpal::traits::DeviceTrait;
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    (inputs, outputs)
}

pub fn default_device_names() -> (Option<String>, Option<String>) {
    use cpal::traits::DeviceTrait;
    let host = cpal::default_host();
    let mic = host.default_input_device().and_then(|d| d.name().ok());
    let out = host.default_output_device().and_then(|d| d.name().ok());
    (mic, out)
}

pub fn start_call_capture(out_path: &Path) -> Result<CaptureHandle> {
    start_call_capture_with(out_path, CaptureDevices::default())
}

pub fn start_call_capture_with(out_path: &Path, devices: CaptureDevices) -> Result<CaptureHandle> {
    let out_path = out_path.to_path_buf();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let async_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let async_error_thread = async_error.clone();

    let join = std::thread::spawn(move || -> Result<PathBuf> {
        let _ae = async_error_thread.clone();
        if let Some(p) = out_path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let spec = WavSpec {
            channels: 2,
            sample_rate: TARGET_SR,
            bits_per_sample: 16,
            sample_format: WavFormat::Int,
        };
        let writer = Arc::new(Mutex::new(Some(WavWriter::create(&out_path, spec)?)));

        let host = cpal::default_host();
        let input_device = pick_input_device(&host, devices.mic_name.as_deref())?;

        let mic_buf = Arc::new(Mutex::new(Vec::<f32>::new()));
        let loop_buf = Arc::new(Mutex::new(Vec::<f32>::new()));

        let mic_stream = build_capture_stream(&input_device, mic_buf.clone(), TARGET_SR, false)?;
        mic_stream.play()?;

        // Loopback source: either a cpal output-as-input loopback OR a
        // per-process WASAPI loopback. Hold both kinds in a generic
        // "keep alive" Box so the stream/thread isn't dropped mid-recording.
        #[allow(dead_code)]
        enum LoopbackHandle {
            Stream(cpal::Stream),
            #[cfg(target_os = "windows")]
            Process(crate::proc_loopback::ProcessLoopbackHandle),
        }

        let _loopback_handle: LoopbackHandle = match devices.loopback {
            LoopbackSource::Device { name } => {
                let dev = pick_loopback_device(&host, name.as_deref())?;
                let s = build_capture_stream(&dev, loop_buf.clone(), TARGET_SR, true)?;
                s.play()?;
                LoopbackHandle::Stream(s)
            }
            #[cfg(target_os = "windows")]
            LoopbackSource::Process { pid, label } => {
                tracing::info!("starting per-process loopback for {label} (pid {pid})");
                let loop_buf_clone = loop_buf.clone();
                let src_sr_slot: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
                let src_sr_set = src_sr_slot.clone();
                let ae_for_proc = _ae.clone();
                let h = crate::proc_loopback::start_capture(
                    pid,
                    move |fmt| {
                        tracing::info!(
                            "process loopback format: {} ch, {} Hz",
                            fmt.channels,
                            fmt.sample_rate
                        );
                        *src_sr_set.lock().unwrap() = Some(fmt.sample_rate);
                    },
                    move |frames, channels| {
                        let src_sr = src_sr_slot.lock().unwrap().unwrap_or(48_000);
                        let mono: Vec<f32> = if channels == 1 {
                            frames.to_vec()
                        } else {
                            frames
                                .chunks(channels as usize)
                                .map(|f| f.iter().sum::<f32>() / channels as f32)
                                .collect()
                        };
                        let resampled = if src_sr == TARGET_SR {
                            mono
                        } else {
                            linear_resample(&mono, src_sr, TARGET_SR)
                        };
                        loop_buf_clone.lock().unwrap().extend(resampled);
                    },
                    move |err| {
                        *ae_for_proc.lock().unwrap() = Some(err);
                    },
                )?;
                LoopbackHandle::Process(h)
            }
            #[cfg(not(target_os = "windows"))]
            LoopbackSource::Process { .. } => {
                return Err(anyhow!("per-process loopback is Windows-only"));
            }
        };

        let writer_for_loop = writer.clone();
        loop {
            if stop_rx.recv_timeout(std::time::Duration::from_millis(50)).is_ok() {
                break;
            }
            interleave_and_write(&mic_buf, &loop_buf, &writer_for_loop)?;
        }

        drop(mic_stream);
        drop(_loopback_handle);
        interleave_and_write(&mic_buf, &loop_buf, &writer)?;
        if let Some(w) = writer.lock().unwrap().take() {
            w.finalize()?;
        }
        Ok(out_path)
    });

    Ok(CaptureHandle {
        stop_tx,
        join: Some(join),
        async_error,
    })
}

fn interleave_and_write(
    mic_buf: &Arc<Mutex<Vec<f32>>>,
    loop_buf: &Arc<Mutex<Vec<f32>>>,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
) -> Result<()> {
    let mut mic = mic_buf.lock().unwrap();
    let mut lp = loop_buf.lock().unwrap();
    let n = mic.len().min(lp.len());
    if n == 0 {
        return Ok(());
    }
    let mic_slice: Vec<f32> = mic.drain(..n).collect();
    let lp_slice: Vec<f32> = lp.drain(..n).collect();
    drop(mic);
    drop(lp);

    let mut w_guard = writer.lock().unwrap();
    let w = w_guard.as_mut().ok_or_else(|| anyhow!("writer closed"))?;
    let mut mic_sq: f64 = 0.0;
    let mut lp_sq: f64 = 0.0;
    for i in 0..n {
        let m = mic_slice[i];
        let l = lp_slice[i];
        mic_sq += (m as f64) * (m as f64);
        lp_sq += (l as f64) * (l as f64);
        w.write_sample(float_to_i16(m))?;
        w.write_sample(float_to_i16(l))?;
    }
    let mic_rms = ((mic_sq / n as f64).sqrt() * 1000.0) as u64;
    let lp_rms = ((lp_sq / n as f64).sqrt() * 1000.0) as u64;
    LAST_MIC_RMS_X1000.store(mic_rms, Ordering::Relaxed);
    LAST_LOOPBACK_RMS_X1000.store(lp_rms, Ordering::Relaxed);
    Ok(())
}

fn float_to_i16(v: f32) -> i16 {
    let clamped = v.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

fn build_capture_stream(
    device: &cpal::Device,
    sink: Arc<Mutex<Vec<f32>>>,
    target_sr: u32,
    _loopback: bool,
) -> Result<Stream> {
    let supported = device
        .default_input_config()
        .with_context(|| format!("default_input_config for {:?}", device.name().ok()))?;
    let src_sr = supported.sample_rate().0;
    let src_channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let err_fn = |e| tracing::error!("audio stream error: {e}");

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                process_frames(data, src_channels, src_sr, target_sr, &sink);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let buf: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                process_frames(&buf, src_channels, src_sr, target_sr, &sink);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let buf: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 - 32768.0) / 32768.0)
                    .collect();
                process_frames(&buf, src_channels, src_sr, target_sr, &sink);
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    let _ = SampleRate(target_sr); // silence unused warning when same-rate
    Ok(stream)
}

fn process_frames(
    interleaved: &[f32],
    channels: usize,
    src_sr: u32,
    target_sr: u32,
    sink: &Arc<Mutex<Vec<f32>>>,
) {
    // Downmix to mono.
    let mono: Vec<f32> = if channels == 1 {
        interleaved.to_vec()
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    let resampled = if src_sr == target_sr {
        mono
    } else {
        linear_resample(&mono, src_sr, target_sr)
    };
    sink.lock().unwrap().extend(resampled);
}

// Lightweight linear-interp resampler. For final-quality work, rubato is wired
// in via Cargo.toml — swap in `SincFixedIn` when a recording is being finalized.
pub fn linear_resample(input: &[f32], src_sr: u32, dst_sr: u32) -> Vec<f32> {
    if input.is_empty() || src_sr == dst_sr {
        return input.to_vec();
    }
    let ratio = dst_sr as f64 / src_sr as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = (i as f64) / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

fn pick_input_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = name {
        if let Ok(it) = host.input_devices() {
            for d in it {
                if d.name().map(|n| n == name).unwrap_or(false) {
                    return Ok(d);
                }
            }
        }
        tracing::warn!("input device '{}' not found, falling back to default", name);
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device (mic)"))
}

#[cfg(target_os = "windows")]
fn pick_loopback_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    // cpal exposes WASAPI loopback by treating an *output* device as an input.
    if let Some(name) = name {
        if let Ok(it) = host.output_devices() {
            for d in it {
                if d.name().map(|n| n == name).unwrap_or(false) {
                    return Ok(d);
                }
            }
        }
        tracing::warn!("output device '{}' not found for loopback, falling back to default", name);
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("no default output device for loopback"))
}

#[cfg(not(target_os = "windows"))]
fn pick_loopback_device(_host: &cpal::Host, _name: Option<&str>) -> Result<cpal::Device> {
    Err(anyhow!("loopback capture only implemented for Windows"))
}
