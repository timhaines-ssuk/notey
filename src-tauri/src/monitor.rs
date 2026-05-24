//! Passive level-only audio monitor. Opens the same cpal streams the real
//! capture path uses, but only updates the RMS atomics in `audio_capture` —
//! no WAV write, no live buffer, no proc-loopback.
//!
//! Used by the Studio page to show meters before the user has hit Record, so
//! they can verify the mic + system audio devices are correct first.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::audio_capture::{LAST_LOOPBACK_RMS_X1000, LAST_MIC_RMS_X1000};

pub struct MonitorHandle {
    _mic: Stream,
    _loop: Stream,
}

unsafe impl Send for MonitorHandle {}

pub fn start_monitor(
    mic_name: Option<&str>,
    loopback_name: Option<&str>,
) -> Result<MonitorHandle> {
    let host = cpal::default_host();
    let mic_dev = pick_input(&host, mic_name)?;
    let out_dev = pick_output(&host, loopback_name)?;

    let mic_stream = build_meter_stream(&mic_dev, true)?;
    let loop_stream = build_meter_stream(&out_dev, false)?;
    mic_stream.play()?;
    loop_stream.play()?;

    Ok(MonitorHandle {
        _mic: mic_stream,
        _loop: loop_stream,
    })
}

fn pick_input(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = name {
        if let Ok(it) = host.input_devices() {
            for d in it {
                if d.name().map(|n| n == name).unwrap_or(false) {
                    return Ok(d);
                }
            }
        }
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))
}

fn pick_output(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = name {
        if let Ok(it) = host.output_devices() {
            for d in it {
                if d.name().map(|n| n == name).unwrap_or(false) {
                    return Ok(d);
                }
            }
        }
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("no default output device"))
}

fn build_meter_stream(device: &cpal::Device, is_mic: bool) -> Result<Stream> {
    use cpal::{SampleFormat, StreamConfig};
    let supported = device.default_input_config()?;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let cfg: StreamConfig = supported.into();
    let acc: Arc<Mutex<RmsAcc>> = Arc::new(Mutex::new(RmsAcc::default()));

    let err_fn = |e| tracing::error!("monitor stream error: {e}");

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &cfg,
            move |data: &[f32], _| feed(data, channels, &acc, is_mic),
            err_fn,
            None,
        )?,
        SampleFormat::I16 => {
            let acc2 = acc.clone();
            device.build_input_stream(
                &cfg,
                move |data: &[i16], _| {
                    let buf: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    feed(&buf, channels, &acc2, is_mic);
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let acc2 = acc.clone();
            device.build_input_stream(
                &cfg,
                move |data: &[u16], _| {
                    let buf: Vec<f32> = data
                        .iter()
                        .map(|s| (*s as f32 - 32768.0) / 32768.0)
                        .collect();
                    feed(&buf, channels, &acc2, is_mic);
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    Ok(stream)
}

#[derive(Default)]
struct RmsAcc {
    sum_sq: f64,
    count: u64,
}

fn feed(interleaved: &[f32], channels: usize, acc: &Arc<Mutex<RmsAcc>>, is_mic: bool) {
    // Downmix to mono on the fly.
    let mut a = acc.lock().unwrap();
    if channels == 1 {
        for &s in interleaved {
            a.sum_sq += (s as f64) * (s as f64);
        }
        a.count += interleaved.len() as u64;
    } else {
        for frame in interleaved.chunks(channels) {
            let m: f32 = frame.iter().sum::<f32>() / channels as f32;
            a.sum_sq += (m as f64) * (m as f64);
            a.count += 1;
        }
    }
    // Flush every ~50ms-worth of samples.
    if a.count >= 2400 {
        let rms = (a.sum_sq / a.count as f64).sqrt();
        a.sum_sq = 0.0;
        a.count = 0;
        let v = ((rms) * 1000.0) as u64;
        if is_mic {
            LAST_MIC_RMS_X1000.store(v, Ordering::Relaxed);
        } else {
            LAST_LOOPBACK_RMS_X1000.store(v, Ordering::Relaxed);
        }
    }
}
