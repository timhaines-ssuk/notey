import { useEffect, useState } from "react";
import { api, SegmentRow } from "../lib/tauri-api";
import SpeakerNamingDialog from "./SpeakerNamingDialog";

export default function TranscriptViewer({
  recordingId,
  onBack,
}: {
  recordingId: number;
  onBack: () => void;
}) {
  const [segments, setSegments] = useState<SegmentRow[]>([]);
  const [namingOpen, setNamingOpen] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api.getSegments(recordingId).then(setSegments).catch((e) => setErr(String(e)));
  }, [recordingId]);

  async function doExport(fmt: "vtt" | "json" | "md") {
    try {
      const out = await api.exportRecording(recordingId, [fmt]);
      alert(`Exported:\n${out.join("\n")}`);
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
        <button onClick={onBack}>← Back</button>
        <h2 style={{ margin: 0 }}>Recording #{recordingId}</h2>
        <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
          <button className="primary" onClick={() => setNamingOpen(true)}>Name speakers</button>
          <button onClick={() => doExport("vtt")}>Export VTT</button>
          <button onClick={() => doExport("json")}>Export JSON</button>
          <button onClick={() => doExport("md")}>Export MD</button>
        </div>
      </div>
      {err && <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>}
      <div className="card">
        {segments.length === 0 && <em>No segments yet.</em>}
        {segments.map((s) => (
          <div key={s.id} style={{ marginBottom: 8 }}>
            <span style={{ color: "#8b8f99", fontFamily: "monospace", marginRight: 8 }}>
              {fmtTime(s.start_seconds)}
            </span>
            <strong>{s.speaker_name ?? `Speaker ${s.speaker_id ?? "?"}`}: </strong>
            <span>{s.text}</span>
          </div>
        ))}
      </div>
      {namingOpen && (
        <SpeakerNamingDialog recordingId={recordingId} onClose={() => setNamingOpen(false)} />
      )}
    </>
  );
}

function fmtTime(s: number) {
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}
