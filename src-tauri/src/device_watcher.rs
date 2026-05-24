//! Watch for USB voice-recorder plug-in.
//!
//! Polls the set of mounted drive letters at 1 Hz and emits a `PluggedDevice`
//! whenever a previously-unseen drive appears containing audio files.
//! Cheap, no admin rights, and avoids RegisterDeviceNotification complexity.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluggedDevice {
    pub mount: PathBuf,
    pub label: Option<String>,
    pub audio_files: Vec<PathBuf>,
}

pub fn start_watch() -> Receiver<PluggedDevice> {
    let (tx, rx) = mpsc::channel(8);

    tokio::spawn(async move {
        // Seed with currently-mounted drives so we don't fire on startup.
        let mut seen: HashSet<PathBuf> = current_drives().into_iter().collect();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let drives = current_drives();
            for d in &drives {
                if !seen.contains(d) {
                    if let Some(dev) = inspect_drive(d) {
                        if tx.send(dev).await.is_err() {
                            return;
                        }
                    }
                }
            }
            seen = drives.into_iter().collect();
        }
    });

    rx
}

#[cfg(target_os = "windows")]
fn current_drives() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::core::PCWSTR;

    // DRIVE_REMOVABLE is 2; the constant lives in different submodules across
    // windows-rs versions, so use the literal.
    const DRIVE_REMOVABLE: u32 = 2;

    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
        if drive_type == DRIVE_REMOVABLE {
            out.push(PathBuf::from(root));
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn current_drives() -> Vec<PathBuf> {
    Vec::new()
}

fn inspect_drive(mount: &PathBuf) -> Option<PluggedDevice> {
    let audio_files = walk_audio(mount).ok()?;
    if audio_files.is_empty() {
        return None;
    }
    let label = volume_label(mount);
    Some(PluggedDevice {
        mount: mount.clone(),
        label,
        audio_files,
    })
}

#[cfg(target_os = "windows")]
fn volume_label(mount: &PathBuf) -> Option<String> {
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
    use windows::core::PCWSTR;
    let root_wide: Vec<u16> = mount
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut name_buf = [0u16; 256];
    let ok = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut name_buf),
            None,
            None,
            None,
            None,
        )
    };
    if ok.is_err() {
        return None;
    }
    let len = name_buf.iter().position(|&c| c == 0).unwrap_or(name_buf.len());
    let s = String::from_utf16_lossy(&name_buf[..len]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(not(target_os = "windows"))]
fn volume_label(_mount: &PathBuf) -> Option<String> {
    None
}

fn walk_audio(root: &PathBuf) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if is_audio(&p) {
                out.push(p);
            }
        }
        if out.len() > 4096 {
            break;
        }
    }
    out.sort();
    Ok(out)
}

fn is_audio(p: &std::path::Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(),
        Some("wav" | "mp3" | "m4a" | "flac" | "ogg")
    )
}
