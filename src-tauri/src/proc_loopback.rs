//! Per-process WASAPI loopback capture.
//!
//! Uses `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_ACTIVATION_PARAMS`
//! (kind = AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK), which is the same
//! API OBS Studio's "Application Audio Capture" source uses on Windows 10
//! build 20348+ / Windows 11.

#![cfg(target_os = "windows")]

use anyhow::{anyhow, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::{implement, IUnknown, Interface, HRESULT, PCWSTR, PROPVARIANT};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::System::Threading::{CreateEventA, SetEvent, WaitForSingleObject};

/// Documented virtual device for process loopback (audioclientactivationparams.h).
const VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK: PCWSTR =
    windows::core::w!("VAD\\Process_Loopback");

// Local PROPVARIANT layout for the VT_BLOB case. Memory-compatible with
// windows_core::PROPVARIANT (which is #[repr(transparent)] over an internal
// #[repr(C)] struct with this exact shape).
#[repr(C)]
struct PropVariantBlob {
    vt: u16,
    w_reserved1: u16,
    w_reserved2: u16,
    w_reserved3: u16,
    // Anonymous union containing a BLOB. On Windows x64 the BLOB is
    // { ULONG cbSize; BYTE *pBlobData; } = 4 bytes + 4 bytes padding + 8 bytes.
    cb_size: u32,
    _pad: u32,
    p_blob_data: *mut u8,
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct CompletionHandler {
    event: HANDLE,
    result: Arc<Mutex<Option<Result<IAudioClient>>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for CompletionHandler_Impl {
    fn ActivateCompleted(
        &self,
        activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let op = match activate_operation {
            Some(o) => o,
            None => {
                *self.result.lock().unwrap() = Some(Err(anyhow!("no activation operation")));
                unsafe { let _ = SetEvent(self.event); }
                return Ok(());
            }
        };

        let mut activate_result: HRESULT = HRESULT(0);
        let mut interface: Option<IUnknown> = None;
        let r = unsafe { op.GetActivateResult(&mut activate_result, &mut interface) };
        if let Err(e) = r {
            *self.result.lock().unwrap() = Some(Err(anyhow!("GetActivateResult: {e}")));
            unsafe { let _ = SetEvent(self.event); }
            return Ok(());
        }
        if activate_result.is_err() {
            *self.result.lock().unwrap() = Some(Err(anyhow!(
                "activation HRESULT: 0x{:08x}",
                activate_result.0 as u32
            )));
            unsafe { let _ = SetEvent(self.event); }
            return Ok(());
        }
        match interface.and_then(|i| i.cast::<IAudioClient>().ok()) {
            Some(client) => *self.result.lock().unwrap() = Some(Ok(client)),
            None => *self.result.lock().unwrap() = Some(Err(anyhow!("no IAudioClient returned"))),
        }
        unsafe {
            let _ = SetEvent(self.event);
        }
        Ok(())
    }
}

pub struct ProcessLoopbackHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ProcessLoopbackHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessLoopbackFormat {
    pub channels: u32,
    pub sample_rate: u32,
}

/// Start process-loopback capture for the given target PID, including its
/// whole process tree (Discord and Teams both have helper processes that
/// actually render audio).
///
/// `on_error` is called from the capture thread if activation fails *after*
/// `start_capture` itself has already returned — useful for the UI to surface
/// the problem instead of recording silence.
pub fn start_capture<F>(
    target_pid: u32,
    on_format: impl FnOnce(ProcessLoopbackFormat) + Send + 'static,
    mut on_frames: F,
    on_error: impl FnOnce(String) + Send + 'static,
) -> Result<ProcessLoopbackHandle>
where
    F: FnMut(&[f32], u32) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let thread = std::thread::Builder::new()
        .name("proc-loopback".into())
        .spawn(move || {
            if let Err(e) = run(target_pid, stop_thread, on_format, &mut on_frames) {
                let msg = format!("process loopback failed: {e:#}");
                tracing::error!("{msg}");
                on_error(msg);
            }
        })?;

    Ok(ProcessLoopbackHandle {
        stop,
        thread: Some(thread),
    })
}

fn run<F>(
    target_pid: u32,
    stop: Arc<AtomicBool>,
    on_format: impl FnOnce(ProcessLoopbackFormat),
    on_frames: &mut F,
) -> Result<()>
where
    F: FnMut(&[f32], u32),
{
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    let prop = PropVariantBlob {
        vt: VT_BLOB.0 as u16,
        w_reserved1: 0,
        w_reserved2: 0,
        w_reserved3: 0,
        cb_size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        _pad: 0,
        p_blob_data: &activation_params as *const _ as *mut u8,
    };

    let event = unsafe { CreateEventA(None, false, false, None)? };
    let result_slot: Arc<Mutex<Option<Result<IAudioClient>>>> = Arc::new(Mutex::new(None));

    let handler: IActivateAudioInterfaceCompletionHandler = CompletionHandler {
        event,
        result: result_slot.clone(),
    }
    .into();

    // SAFETY: PROPVARIANT is #[repr(transparent)] over a #[repr(C)] inner
    // struct whose layout matches PropVariantBlob for the VT_BLOB case.
    let prop_ptr: *const PROPVARIANT = &prop as *const PropVariantBlob as *const PROPVARIANT;

    let op: IActivateAudioInterfaceAsyncOperation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(prop_ptr),
            &handler,
        )?
    };
    // Hold `op` until the completion handler fires so the operation isn't released early.
    let _op_keep_alive = op;

    let wait = unsafe { WaitForSingleObject(event, 5000) };
    if wait != WAIT_OBJECT_0 {
        return Err(anyhow!("activation timed out (process not producing audio yet?)"));
    }

    let audio_client = result_slot
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| anyhow!("completion handler did not produce a result"))??;

    // Required format for process loopback: 32-bit float, 44.1 or 48 kHz, stereo.
    let fmt = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 2 * 4,
            nBlockAlign: 2 * 4,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: 3, // FL | FR
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };

    let buffer_duration_100ns: i64 = 20 * 10_000; // 20 ms
    unsafe {
        audio_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                buffer_duration_100ns,
                0,
                &fmt as *const _ as *const WAVEFORMATEX,
                None,
            )
            .context("IAudioClient::Initialize for process loopback")?;
    }

    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService()? };
    let buffer_event = unsafe { CreateEventA(None, false, false, None)? };
    unsafe { audio_client.SetEventHandle(buffer_event)? };

    let sample_rate = fmt.Format.nSamplesPerSec;
    let channels = fmt.Format.nChannels as u32;
    on_format(ProcessLoopbackFormat { channels, sample_rate });

    unsafe { audio_client.Start()? };

    let mut scratch: Vec<f32> = Vec::with_capacity(48_000);
    while !stop.load(Ordering::SeqCst) {
        let _ = unsafe { WaitForSingleObject(buffer_event, 100) };

        loop {
            let packet_frames = match unsafe { capture_client.GetNextPacketSize() } {
                Ok(n) => n,
                Err(_) => break,
            };
            if packet_frames == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut frames_available: u32 = 0;
            let mut flags: u32 = 0;
            let getbuf = unsafe {
                capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut frames_available,
                    &mut flags,
                    None,
                    None,
                )
            };
            if getbuf.is_err() {
                break;
            }

            let n_samples = (frames_available as usize) * (channels as usize);
            scratch.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                scratch.resize(n_samples, 0.0);
            } else if !data_ptr.is_null() {
                let src = unsafe { std::slice::from_raw_parts(data_ptr as *const f32, n_samples) };
                scratch.extend_from_slice(src);
            }
            on_frames(&scratch, channels);

            unsafe {
                let _ = capture_client.ReleaseBuffer(frames_available);
            }
        }
    }

    unsafe {
        let _ = audio_client.Stop();
    }
    Ok(())
}

/// Find the PID for a process whose executable name (case-insensitive) matches
/// any of the given names. Returns the first match.
pub fn find_pid(names: &[&str]) -> Option<u32> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All);
    for p in sys.processes().values() {
        let pname = p.name().to_string_lossy().to_lowercase();
        if names.iter().any(|n| pname == n.to_lowercase()) {
            return Some(p.pid().as_u32());
        }
    }
    None
}

/// Enumerate every process currently holding an audio session on the default
/// render device. This is the same list OBS shows in its Application Audio
/// Capture source dropdown. Each entry is suitable as a process-loopback target.
pub fn list_audio_sessions() -> Vec<AudioSession> {
    match enumerate_audio_sessions() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("audio session enumeration failed: {e:?}");
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioSession {
    pub pid: u32,
    pub display_name: String,
    pub process_name: String,
}

fn enumerate_audio_sessions() -> Result<Vec<AudioSession>> {
    use windows::Win32::Media::Audio::{
        eRender, eMultimedia, IAudioSessionEnumerator, IAudioSessionManager2, IMMDeviceEnumerator,
        MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let session_mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions: IAudioSessionEnumerator = session_mgr.GetSessionEnumerator()?;

        let count = sessions.GetCount()?;
        let mut out = Vec::new();
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All);

        for i in 0..count {
            let Ok(ctrl) = sessions.GetSession(i) else { continue };
            let Ok(ctrl2) = ctrl.cast::<windows::Win32::Media::Audio::IAudioSessionControl2>() else { continue };
            let Ok(pid) = ctrl2.GetProcessId() else { continue };
            if pid == 0 {
                continue;
            }
            // The Windows SystemSoundsSession reports pid=0 sometimes; filter that.

            let proc_name = sys
                .process(sysinfo::Pid::from_u32(pid))
                .map(|p| p.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("pid {pid}"));

            let display = ctrl
                .GetDisplayName()
                .ok()
                .and_then(|s| {
                    let st = s.to_string().ok();
                    if let Some(st) = &st {
                        if !st.is_empty() {
                            return Some(st.clone());
                        }
                    }
                    st
                })
                .unwrap_or_else(|| {
                    // Trim ".exe" and title-case for prettier display.
                    proc_name
                        .strip_suffix(".exe")
                        .or_else(|| proc_name.strip_suffix(".EXE"))
                        .unwrap_or(&proc_name)
                        .to_string()
                });

            // De-duplicate per PID — multi-stream apps register multiple sessions.
            if out.iter().any(|a: &AudioSession| a.pid == pid) {
                continue;
            }
            out.push(AudioSession {
                pid,
                display_name: display,
                process_name: proc_name,
            });
        }
        out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
        Ok(out)
    }
}
