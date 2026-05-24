// Spike #1 (§11): WAV → whisper → sherpa → speaker-labeled transcript.
// Run: cargo run --bin spike-asr --features cuda -- input.wav models/whisper/ggml-medium.en.bin
//   (or pass --features ml for CPU-only)

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let audio = PathBuf::from(args.next().expect("audio.wav"));
    let whisper = PathBuf::from(args.next().expect("ggml-*.bin"));

    println!("Transcribing {} with {}...", audio.display(), whisper.display());
    let segs = notetaker_lib::transcribe::transcribe_file(
        &whisper,
        &audio,
        notetaker_lib::transcribe::Mode::Finalize,
        0,
    )?;
    for s in &segs {
        println!(
            "[{:>7.2} → {:>7.2}] {}",
            s.start_seconds, s.end_seconds, s.text
        );
    }
    println!("\n{} segments.", segs.len());
    Ok(())
}
