import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
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
  const [summary, setSummary] = useState<string | null>(null);
  const [rolling, setRolling] = useState<string | null>(null);
  const [stage, setStage] = useState<string | null>(null);
  const [namingOpen, setNamingOpen] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function refreshAll() {
    try {
      const [segs, sm, rs] = await Promise.all([
        api.getSegments(recordingId),
        api.getSummary(recordingId),
        api.getRollingSummary(recordingId),
      ]);
      setSegments(segs);
      setSummary(sm);
      setRolling(rs);
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => {
    refreshAll();
    const ticker = setInterval(refreshAll, 3000);
    const unStage = listen<[string, number]>("pipeline-stage", (e) => {
      const [s, rid] = e.payload as any;
      if (rid === recordingId) {
        setStage(s);
        if (s === "complete") refreshAll();
      }
    });
    const unErr = listen<string>("pipeline-error", (e) => {
      if (typeof e.payload === "string" && e.payload.startsWith(`${recordingId}:`)) {
        setErr(e.payload);
      }
    });
    const unSummErr = listen<string>("summarize-error", (e) => {
      if (typeof e.payload === "string" && e.payload.startsWith(`${recordingId}:`)) {
        setErr(e.payload);
      }
    });
    return () => {
      clearInterval(ticker);
      unStage.then((u) => u());
      unErr.then((u) => u());
      unSummErr.then((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
        {stage && stage !== "complete" && (
          <span className="status-pill" style={{ background: "#3c6eff" }}>
            {stage}…
          </span>
        )}
        <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
          <button className="primary" onClick={() => setNamingOpen(true)}>Name speakers</button>
          <button onClick={() => doExport("vtt")}>Export VTT</button>
          <button onClick={() => doExport("json")}>Export JSON</button>
          <button onClick={() => doExport("md")}>Export MD</button>
        </div>
      </div>
      {err && <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>}

      {summary && (
        <div className="card">
          <h3 style={{ marginTop: 0 }}>Summary</h3>
          <pre style={{ whiteSpace: "pre-wrap", margin: 0, fontFamily: "inherit" }}>{summary}</pre>
        </div>
      )}
      {!summary && rolling && (
        <div className="card">
          <h3 style={{ marginTop: 0 }}>Rolling summary (in progress)</h3>
          <pre style={{ whiteSpace: "pre-wrap", margin: 0, fontFamily: "inherit" }}>{rolling}</pre>
        </div>
      )}

      <div className="card">
        <h3 style={{ marginTop: 0 }}>Transcript</h3>
        {segments.length === 0 ? (
          <em>{stage ? `Waiting for pipeline (${stage})…` : "No segments yet — pipeline may still be running or models still downloading."}</em>
        ) : (
          segments.map((s) => (
            <div key={s.id} style={{ marginBottom: 8 }}>
              <span style={{ color: "#8b8f99", fontFamily: "monospace", marginRight: 8 }}>
                {fmtTime(s.start_seconds)}
              </span>
              <strong>{s.speaker_name ?? `Speaker ${s.speaker_id ?? "?"}`}: </strong>
              <span>{s.text}</span>
            </div>
          ))
        )}
      </div>

      {namingOpen && (
        <SpeakerNamingDialog recordingId={recordingId} onClose={() => { setNamingOpen(false); refreshAll(); }} />
      )}
    </>
  );
}

function fmtTime(s: number) {
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}
