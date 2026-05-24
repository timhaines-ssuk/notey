use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const ROLLING_CHUNK_SECONDS: f64 = 300.0; // 5 minutes per §4

#[derive(Serialize)]
struct GenRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct GenResponse {
    response: String,
}

pub async fn ollama_generate(base_url: &str, model: &str, prompt: &str) -> Result<String> {
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let body = GenRequest {
        model,
        prompt,
        stream: false,
    };
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("Ollama POST failed (is Ollama running on http://localhost:11434?)")?
        .error_for_status()?
        .json::<GenResponse>()
        .await?;
    Ok(resp.response.trim().to_string())
}

pub fn chunk_prompt(speaker_lines: &str) -> String {
    format!(
        "You are summarising part of a meeting transcript. Output 3-6 bullet \
         points covering the key decisions, action items, and topics discussed.\n\n\
         Transcript:\n{speaker_lines}\n\n\
         Summary (bullets, no preamble):"
    )
}

pub fn rolling_prompt(previous: &str, new_bullets: &str) -> String {
    format!(
        "You maintain a running summary of a meeting. Update the running \
         summary by integrating new bullets. Stay concise; don't repeat.\n\n\
         Current running summary:\n{previous}\n\n\
         New bullets to integrate:\n{new_bullets}\n\n\
         Updated running summary:"
    )
}

pub fn final_prompt(all_bullets: &str, speakers: &[String]) -> String {
    let participants = speakers.join(", ");
    format!(
        "Below are bullet-point summaries of consecutive segments of a meeting \
         with participants: {participants}. Produce a clean final summary with \
         these sections in Markdown: Overview, Decisions, Action items (with \
         owner where stated), Open questions.\n\n\
         Segment bullets:\n{all_bullets}\n\n\
         Final summary:"
    )
}

/// Group segments into ROLLING_CHUNK_SECONDS-long windows, breaking on silence
/// gaps where possible (any gap > 1.0s ends a chunk early so we don't cut in
/// the middle of a sentence).
pub fn group_into_chunks<'a, S: AsRef<crate::db::SegmentRow>>(
    segments: &'a [S],
) -> Vec<(f64, f64, &'a [S])> {
    let mut out = Vec::new();
    if segments.is_empty() {
        return out;
    }
    let mut chunk_start = segments[0].as_ref().start_seconds;
    let mut start_idx = 0usize;
    for i in 1..segments.len() {
        let prev = segments[i - 1].as_ref();
        let cur = segments[i].as_ref();
        let gap = cur.start_seconds - prev.end_seconds;
        let elapsed = cur.start_seconds - chunk_start;
        if elapsed >= ROLLING_CHUNK_SECONDS && gap > 1.0 {
            out.push((chunk_start, prev.end_seconds, &segments[start_idx..i]));
            chunk_start = cur.start_seconds;
            start_idx = i;
        } else if elapsed >= ROLLING_CHUNK_SECONDS * 1.5 {
            // Hard cutoff so a chunk doesn't grow unboundedly when there's no silence.
            out.push((chunk_start, prev.end_seconds, &segments[start_idx..i]));
            chunk_start = cur.start_seconds;
            start_idx = i;
        }
    }
    let last = segments.last().unwrap().as_ref();
    out.push((chunk_start, last.end_seconds, &segments[start_idx..]));
    out
}

impl AsRef<crate::db::SegmentRow> for crate::db::SegmentRow {
    fn as_ref(&self) -> &crate::db::SegmentRow {
        self
    }
}

pub fn format_segments_for_prompt(segs: &[crate::db::SegmentRow]) -> String {
    let mut buf = String::new();
    for s in segs {
        let speaker = s.speaker_name.as_deref().unwrap_or("Unknown");
        buf.push_str(&format!("{speaker}: {}\n", s.text.trim()));
    }
    buf
}
