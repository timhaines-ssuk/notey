import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, RecordingRow, SegmentRow } from "../lib/tauri-api";

interface AudioSession {
  pid: number;
  display_name: string;
  process_name: string;
}

export default function Studio({ onOpenRecording }: { onOpenRecording: (id: number) => void }) {
  const [levels, setLevels] = useState<[number, number]>([0, 0]);
  const [capturing, setCapturing] = useState(false);
  const [recId, setRecId] = useState<number | null>(null);
  const [segments, setSegments] = useState<SegmentRow[]>([]);
  const [stage, setStage] = useState<string | null>(null);
  const [callApp, setCallApp] = useState<"discord" | "teams" | "none">("none");
  const [err, setErr] = useState<string | null>(null);
  const [liveStatus, setLiveStatus] = useState<string | null>(null);
  const [settings, setSettings] = useState<Record<string, string>>({});
  const [sessions, setSessions] = useState<AudioSession[]>([]);
  const [duration, setDuration] = useState(0);
  const [recent, setRecent] = useState<RecordingRow[]>([]);
  const transcriptScroll = useRef<HTMLDivElement>(null);
  const startedAt = useRef<number | null>(null);

  // --- Always-on level monitor when not capturing ---
  useEffect(() => {
    let alive = true;
    (async () => {
      if (capturing) return;
      try { await api.startMonitorLevels(); } catch (e) { setErr(String(e)); }
      if (!alive) await api.stopMonitorLevels().catch(() => {});
    })();
    return () => {
      alive = false;
      if (!capturing) api.stopMonitorLevels().catch(() => {});
    };
  }, [capturing, settings.device_mic, settings.device_loopback]);

  // --- Level polling ---
  useEffect(() => {
    const t = setInterval(async () => {
      try { setLevels(await api.captureLevels()); } catch {}
    }, 150);
    return () => clearInterval(t);
  }, []);

  // --- Settings + processes load ---
  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
    api.listAudioSessions().then(setSessions).catch(() => {});
    api.detectCallApp().then(setCallApp).catch(() => {});
    refreshRecent();
    const t = setInterval(() => {
      api.detectCallApp().then(setCallApp).catch(() => {});
      refreshRecent();
    }, 4000);
    return () => clearInterval(t);
  }, []);

  // --- Duration ticker ---
  useEffect(() => {
    if (!capturing) return;
    startedAt.current = Date.now();
    setDuration(0);
    const t = setInterval(() => {
      if (startedAt.current) setDuration((Date.now() - startedAt.current) / 1000);
    }, 250);
    return () => clearInterval(t);
  }, [capturing]);

  // --- Live segment + stage listeners ---
  useEffect(() => {
    const unSeg = listen("live-segment", () => { reloadSegments(); });
    const unStage = listen<[string, number]>("pipeline-stage", (e) => {
      const [s, rid] = e.payload as any;
      if (rid === recId) {
        setStage(s);
        if (s === "complete") {
          reloadSegments();
          refreshRecent();
        }
      }
    });
    const unErr = listen<string>("pipeline-error", (e) => setErr(String(e.payload)));
    const unLiveStatus = listen<string>("live-status", (e) => setLiveStatus(String(e.payload)));
    return () => {
      unSeg.then((u) => u());
      unStage.then((u) => u());
      unErr.then((u) => u());
      unLiveStatus.then((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recId]);

  // --- Segment polling while capturing ---
  useEffect(() => {
    if (!recId) return;
    const t = setInterval(reloadSegments, 1500);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recId]);

  async function reloadSegments() {
    if (!recId) return;
    try {
      const segs = await api.getSegments(recId);
      setSegments(segs);
      // Auto-scroll to bottom
      if (transcriptScroll.current) {
        transcriptScroll.current.scrollTop = transcriptScroll.current.scrollHeight;
      }
    } catch {}
  }

  async function refreshRecent() {
    try {
      const rows = await api.listRecordings();
      setRecent(rows.slice(0, 8));
    } catch {}
  }

  async function updateSetting(key: string, value: string) {
    setSettings({ ...settings, [key]: value });
    try { await api.setSetting(key, value); } catch (e) { setErr(String(e)); }
  }

  async function toggleCapture() {
    setErr(null);
    setLiveStatus(null);
    try {
      if (capturing) {
        await api.stopCallCapture();
        setCapturing(false);
        // recId stays set so transcript continues to refresh through pipeline
        startedAt.current = null;
      } else {
        setSegments([]);
        const id = await api.startCallCapture();
        setRecId(id);
        setCapturing(true);
        setStage("recording");
      }
    } catch (e) {
      setErr(String(e));
    }
  }

  const source = settings.loopback_source ?? "default";
  const mic = settings.device_mic ?? "";
  const loopDevice = settings.device_loopback ?? "";

  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16, height: "calc(100vh - 48px)" }}>
      {/* Left column: controls + meters */}
      <div style={{ display: "flex", flexDirection: "column", gap: 12, overflow: "auto" }}>
        <div className="card">
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
            <h2 style={{ margin: 0 }}>Capture</h2>
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              {callApp !== "none" && (
                <span className="status-pill" style={{ background: "#3c6eff" }}>
                  {callApp === "discord" ? "Discord" : "Teams"} running
                </span>
              )}
              {capturing && (
                <span style={{ fontFamily: "monospace", color: "#8b8f99" }}>
                  {fmtDuration(duration)}
                </span>
              )}
              <button
                className="primary"
                style={{ minWidth: 140, fontWeight: "bold" }}
                onClick={toggleCapture}
              >
                {capturing ? "■ Stop" : "● Start capture"}
              </button>
            </div>
          </div>
        </div>

        <div className="card">
          <h3 style={{ marginTop: 0 }}>Audio levels{capturing ? "" : " (preview)"}</h3>
          <Meter label="Mic (you)" value={levels[0]} />
          <Meter label="System / loopback" value={levels[1]} />
          {!capturing && (levels[0] < 0.001 && levels[1] < 0.001) && (
            <div style={{ color: "#8b8f99", marginTop: 6, fontSize: 12 }}>
              Both meters are flat. Talk into your mic and play any audio — both should move.
              If they don't, change the mic or loopback source below.
            </div>
          )}
        </div>

        <div className="card">
          <h3 style={{ marginTop: 0 }}>Mic</h3>
          <select
            value={mic}
            onChange={(e) => updateSetting("device_mic", e.target.value)}
            style={{ width: "100%" }}
          >
            <option value="">Default input</option>
          </select>
          <div style={{ color: "#8b8f99", fontSize: 12, marginTop: 4 }}>
            Use Settings → Audio for the full device picker if needed.
          </div>
        </div>

        <div className="card">
          <h3 style={{ marginTop: 0 }}>Loopback source</h3>
          <select
            value={source}
            onChange={(e) => updateSetting("loopback_source", e.target.value)}
            style={{ width: "100%" }}
          >
            <option value="default">Default output device (whole system)</option>
            <option value="discord">Discord (process loopback)</option>
            <option value="teams">Microsoft Teams (process loopback)</option>
            {sessions.map((s) => (
              <option key={s.pid} value={`pid:${s.pid}`}>
                {s.display_name} (pid {s.pid})
              </option>
            ))}
          </select>
          {source === "default" && (
            <select
              value={loopDevice}
              onChange={(e) => updateSetting("device_loopback", e.target.value)}
              style={{ width: "100%", marginTop: 8 }}
            >
              <option value="">Default output</option>
            </select>
          )}
        </div>

        <div className="card">
          <h3 style={{ marginTop: 0 }}>Recent recordings</h3>
          {recent.length === 0 && <em>No recordings yet.</em>}
          {recent.map((r) => (
            <div
              key={r.id}
              className="recording-row"
              style={{ marginBottom: 6 }}
              onClick={() => onOpenRecording(r.id)}
            >
              <div>
                <div>{r.source_filename ?? `Recording #${r.id}`}</div>
                <div className="meta">
                  {r.source} · {fmtDur2(r.duration_seconds)}
                </div>
              </div>
              <span className={`status-pill ${r.status}`}>{r.status}</span>
            </div>
          ))}
        </div>
      </div>

      {/* Right column: live transcript */}
      <div style={{ display: "flex", flexDirection: "column", gap: 12, minHeight: 0 }}>
        <div className="card" style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <h3 style={{ margin: 0 }}>
              Transcript
              {recId && <span style={{ color: "#8b8f99", fontSize: 12, fontWeight: "normal" }}>  #{recId}</span>}
            </h3>
            {stage && (
              <span className="status-pill" style={{ background: stage === "complete" ? "#214d2a" : "#3c6eff" }}>
                {stage}
              </span>
            )}
          </div>
          {liveStatus && (
            <div style={{ color: "#e8b1b1", marginTop: 6, fontSize: 12 }}>{liveStatus}</div>
          )}
          {err && (
            <div style={{ color: "#e8b1b1", marginTop: 6, fontSize: 12 }}>{err}</div>
          )}
          <div
            ref={transcriptScroll}
            style={{
              marginTop: 8,
              flex: 1,
              overflow: "auto",
              background: "#1b1c1f",
              padding: 10,
              borderRadius: 6,
              border: "1px solid #2a2c31",
              minHeight: 0,
            }}
          >
            {segments.length === 0 ? (
              <em style={{ color: "#8b8f99" }}>
                {capturing
                  ? "Listening… first live segment usually lands ~10 s in."
                  : "Hit Start capture to begin. Live transcript will appear here."}
              </em>
            ) : (
              segments.map((s) => (
                <div key={s.id} style={{ marginBottom: 6 }}>
                  <span style={{ color: "#8b8f99", fontFamily: "monospace", marginRight: 8 }}>
                    {fmtTime(s.start_seconds)}
                  </span>
                  <strong>
                    {s.speaker_name ?? (s.speaker_id != null ? `Speaker ${s.speaker_id}` : "—")}:
                  </strong>{" "}
                  <span>{s.text}</span>
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function Meter({ label, value }: { label: string; value: number }) {
  const pct = Math.min(100, Math.round(value * 400));
  const active = value > 0.01;
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
        <span>{label}</span>
        <span style={{ fontFamily: "monospace" }}>{value.toFixed(3)}</span>
      </div>
      <div style={{ height: 8, background: "#3a3d44", borderRadius: 4, overflow: "hidden" }}>
        <div style={{ width: `${pct}%`, height: "100%", background: active ? "#3c6eff" : "#555", transition: "width 80ms" }} />
      </div>
    </div>
  );
}

function fmtTime(s: number) {
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}
function fmtDuration(s: number) {
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}
function fmtDur2(s: number | null) {
  if (s == null) return "—";
  return fmtDuration(s);
}
