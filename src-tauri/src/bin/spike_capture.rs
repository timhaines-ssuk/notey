// Spike #2 (§11): starts WASAPI loopback + default mic, writes stereo WAV.
// Run: cargo run --bin spike-capture -- out.wav 30
// (records for the given number of seconds, default 30)

use std::path::PathBuf;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("capture.wav"));
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);

    println!("Capturing to {} for {secs}s…", path.display());
    let handle = notetaker_lib::audio_capture::start_call_capture(&path)?;
    std::thread::sleep(Duration::from_secs(secs));
    let out = handle.stop()?;
    println!("Wrote {}", out.display());
    Ok(())
}
