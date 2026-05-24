use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Intel,
    Amd,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Cuda,
    DirectML,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: GpuVendor,
    pub vram_mb: Option<u32>,
    pub cuda_capable: bool,
    pub directml_capable: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HardwareProfile {
    pub cpu_cores: usize,
    pub total_ram_gb: u32,
    pub gpus: Vec<GpuInfo>,
    pub recommended_backend: Backend,
    pub recommended_whisper_live: String,
    pub recommended_whisper_finalize: String,
    pub recommended_llm: String,
}

pub fn detect() -> Result<HardwareProfile> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let cpu_cores = num_cpus::get_cpu_count();
    let total_ram_gb = (sys.total_memory() / 1024 / 1024 / 1024) as u32;

    let mut gpus = Vec::new();
    gpus.extend(query_nvidia_smi().unwrap_or_default());
    if gpus.is_empty() {
        gpus.extend(query_wmi_gpus().unwrap_or_default());
    } else {
        for g in query_wmi_gpus().unwrap_or_default() {
            if !gpus.iter().any(|n| n.name == g.name) {
                gpus.push(g);
            }
        }
    }

    let recommended_backend = if gpus.iter().any(|g| g.cuda_capable && g.vram_mb.unwrap_or(0) >= 4096) {
        Backend::Cuda
    } else if gpus.iter().any(|g| g.directml_capable) {
        Backend::DirectML
    } else {
        Backend::Cpu
    };

    let max_cuda_vram = gpus
        .iter()
        .filter(|g| g.cuda_capable)
        .filter_map(|g| g.vram_mb)
        .max()
        .unwrap_or(0);

    let (whisper_live, whisper_finalize) = pick_whisper(&recommended_backend, max_cuda_vram, cpu_cores);
    let llm = pick_llm(&recommended_backend, max_cuda_vram, total_ram_gb);

    Ok(HardwareProfile {
        cpu_cores,
        total_ram_gb,
        gpus,
        recommended_backend,
        recommended_whisper_live: whisper_live.into(),
        recommended_whisper_finalize: whisper_finalize.into(),
        recommended_llm: llm.into(),
    })
}

fn pick_whisper(backend: &Backend, cuda_vram_mb: u32, cpu_cores: usize) -> (&'static str, &'static str) {
    match backend {
        Backend::Cuda => {
            if cuda_vram_mb >= 16_000 {
                ("large-v3-turbo", "large-v3")
            } else if cuda_vram_mb >= 8_000 {
                ("medium", "large-v3")
            } else {
                ("small", "medium")
            }
        }
        Backend::DirectML => ("base", "small"),
        Backend::Cpu => {
            if cpu_cores >= 8 {
                ("base.en", "medium.en")
            } else {
                ("base.en", "small.en")
            }
        }
    }
}

fn pick_llm(backend: &Backend, cuda_vram_mb: u32, ram_gb: u32) -> &'static str {
    match backend {
        Backend::Cuda if cuda_vram_mb >= 16_000 => "qwen2.5:14b",
        Backend::Cuda if cuda_vram_mb >= 8_000 => "qwen2.5:7b",
        Backend::Cuda => "llama3.2:3b",
        _ if ram_gb >= 32 => "qwen2.5:7b",
        _ => "llama3.2:3b",
    }
}

fn query_nvidia_smi() -> Result<Vec<GpuInfo>> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output()
        .context("nvidia-smi not on PATH")?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut gpus = Vec::new();
    for line in text.lines() {
        let parts: Vec<_> = line.split(',').map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        let name = parts[0].to_string();
        let vram_mb = parts[1].parse::<u32>().ok();
        gpus.push(GpuInfo {
            name,
            vendor: GpuVendor::Nvidia,
            vram_mb,
            cuda_capable: true,
            directml_capable: true,
        });
    }
    Ok(gpus)
}

#[cfg(target_os = "windows")]
fn query_wmi_gpus() -> Result<Vec<GpuInfo>> {
    use serde::Deserialize;
    use wmi::{COMLibrary, WMIConnection};

    #[derive(Deserialize, Debug)]
    #[serde(rename = "Win32_VideoController")]
    #[serde(rename_all = "PascalCase")]
    struct Win32VideoController {
        name: Option<String>,
        adapter_ram: Option<u64>,
    }

    let com = COMLibrary::new()?;
    let wmi_con = WMIConnection::new(com)?;
    let results: Vec<Win32VideoController> = wmi_con.query()?;
    Ok(results
        .into_iter()
        .filter_map(|r| {
            let name = r.name?;
            let vendor = classify_vendor(&name);
            let vram_mb = r.adapter_ram.map(|b| (b / 1024 / 1024) as u32);
            Some(GpuInfo {
                cuda_capable: matches!(vendor, GpuVendor::Nvidia),
                directml_capable: !matches!(vendor, GpuVendor::Unknown),
                name,
                vendor,
                vram_mb,
            })
        })
        .collect())
}

#[cfg(not(target_os = "windows"))]
fn query_wmi_gpus() -> Result<Vec<GpuInfo>> {
    Ok(vec![])
}

fn classify_vendor(name: &str) -> GpuVendor {
    let n = name.to_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("tesla") {
        GpuVendor::Nvidia
    } else if n.contains("intel") {
        GpuVendor::Intel
    } else if n.contains("amd") || n.contains("radeon") {
        GpuVendor::Amd
    } else {
        GpuVendor::Unknown
    }
}

mod num_cpus {
    pub fn get_cpu_count() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}
