import { useEffect, useState } from "react";
import { api, RecordingRow } from "../lib/tauri-api";
import { manualImport } from "./DevicePluggedDialog";

export default function RecordingsList({ onOpen }: { onOpen: (id: number) => void }) {
  const [rows, setRows] = useState<RecordingRow[]>([]);
  const [capturing, setCapturing] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [levels, setLevels] = useState<[number, number]>([0, 0]);
  const [callApp, setCallApp] = useState<"discord" | "teams" | "none">("none");

  useEffect(() => {
    const t = setInterval(async () => {
      try {
        setCallApp(await api.detectCallApp());
      } catch {}
    }, 3000);
    api.detectCallApp().then(setCallApp).catch(() => {});
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    if (!capturing) return;
    const t = setInterval(async () => {
      try {
        setLevels(await api.captureLevels());
        const e = await api.getCaptureError();
        if (e) setErr(e);
      } catch {}
    }, 200);
    return () => clearInterval(t);
  }, [capturing]);

  async function refresh() {
    try {
      setRows(await api.listRecordings());
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, []);

  async function toggle() {
    try {
      if (capturing) {
        await api.stopCallCapture();
        setCapturing(false);
      } else {
        await api.startCallCapture();
        setCapturing(true);
      }
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <h2>Recordings</h2>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          {callApp !== "none" && !capturing && (
            <span className="status-pill" style={{ background: "#3c6eff" }}>
              {callApp === "discord" ? "Discord" : "Teams"} detected
            </span>
          )}
          <button onClick={async () => {
            try {
              const paths = await manualImport();
              if (paths.length) {
                await api.importRecordings(paths);
                refresh();
              }
            } catch (e) {
              setErr(String(e));
            }
          }}>Import files…</button>
          <button className="primary" onClick={toggle}>
            {capturing ? "Stop capture" : "Start call capture"}
          </button>
        </div>
      </div>
      {capturing && (
        <div className="card">
          <strong>Capturing.</strong> Talk and play audio to verify both meters are moving:
          <Meter label="Mic (you)" value={levels[0]} />
          <Meter label="System audio (Discord/Teams)" value={levels[1]} />
          {levels[1] < 0.005 && (
            <div style={{ color: "#e8b1b1", marginTop: 4 }}>
              System audio meter is flat — check that Windows' default playback device matches where Discord is playing.
            </div>
          )}
        </div>
      )}
      {err && <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>}
      <div className="recordings-list">
        {rows.length === 0 && <div className="card">No recordings yet.</div>}
        {rows.map((r) => (
          <div key={r.id} className="recording-row" onClick={() => onOpen(r.id)}>
            <div>
              <div>{r.source_filename ?? `Recording #${r.id}`}</div>
              <div className="meta">
                {r.source} · {r.app_name ?? "—"} · {formatDuration(r.duration_seconds)} · {r.imported_at}
              </div>
            </div>
            <div className={`status-pill ${r.status}`}>{r.status}</div>
          </div>
        ))}
      </div>
    </>
  );
}

function Meter({ label, value }: { label: string; value: number }) {
  const pct = Math.min(100, Math.round(value * 400));
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
        <span>{label}</span>
        <span style={{ fontFamily: "monospace" }}>{value.toFixed(3)}</span>
      </div>
      <div style={{ height: 6, background: "#3a3d44", borderRadius: 3, overflow: "hidden" }}>
        <div style={{ width: `${pct}%`, height: "100%", background: value > 0.01 ? "#3c6eff" : "#555" }} />
      </div>
    </div>
  );
}

function formatDuration(s: number | null): string {
  if (s == null) return "—";
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = Math.floor(s % 60);
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}
function pad(n: number) { return n.toString().padStart(2, "0"); }
