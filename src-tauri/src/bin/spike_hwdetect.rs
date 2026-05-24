// Spike #3 (§11): print detected hardware + recommended models.
// Run: cargo run --bin spike-hwdetect

fn main() -> anyhow::Result<()> {
    let hw = notetaker_lib::hardware::detect()?;
    println!("CPU cores:        {}", hw.cpu_cores);
    println!("Total RAM:        {} GB", hw.total_ram_gb);
    println!("GPUs:");
    for g in &hw.gpus {
        println!(
            "  - {:?} | {} | {} MB | cuda={} dml={}",
            g.vendor,
            g.name,
            g.vram_mb.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            g.cuda_capable,
            g.directml_capable
        );
    }
    println!("Recommended backend:    {:?}", hw.recommended_backend);
    println!("Recommended whisper live:     {}", hw.recommended_whisper_live);
    println!("Recommended whisper finalize: {}", hw.recommended_whisper_finalize);
    println!("Recommended LLM:        {}", hw.recommended_llm);
    Ok(())
}
