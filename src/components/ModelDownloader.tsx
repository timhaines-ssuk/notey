import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, DownloadProgress, ModelsStatus } from "../lib/tauri-api";

export default function ModelDownloader({ onDone }: { onDone: () => void }) {
  const [status, setStatus] = useState<ModelsStatus | null>(null);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api.modelsStatus().then(setStatus).catch((e) => setErr(String(e)));
    const un = listen<DownloadProgress>("download-progress", (e) =>
      setProgress((p) => ({ ...p, [e.payload.label]: e.payload })),
    );
    return () => {
      un.then((u) => u());
    };
  }, []);

  async function go() {
    setBusy(true);
    setErr(null);
    try {
      await api.downloadModels();
      const next = await api.modelsStatus();
      setStatus(next);
      if (allPresent(next)) onDone();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (!status) return <div>Checking models…</div>;
  if (allPresent(status)) {
    return (
      <div className="card">
        <h3>All models installed</h3>
        <button onClick={onDone}>Continue</button>
      </div>
    );
  }

  return (
    <>
      <h2>First-run model download</h2>
      <div className="card">
        <p>The following models need to be downloaded. Total ~2 GB on first run; cached for future runs.</p>
        <ul>
          <li>
            Whisper live (<code>{status.whisper_live}</code>):{" "}
            {status.whisper_live_present ? "present" : "missing"}
          </li>
          <li>
            Whisper finalize (<code>{status.whisper_finalize}</code>):{" "}
            {status.whisper_finalize_present ? "present" : "missing"}
          </li>
          <li>
            Sherpa speaker embedding: {status.sherpa_embedding_present ? "present" : "missing"}
          </li>
          <li>
            Sherpa segmentation: {status.sherpa_segmentation_present ? "present" : "missing"}
          </li>
        </ul>
        <button className="primary" onClick={go} disabled={busy}>
          {busy ? "Downloading…" : "Download missing models"}
        </button>
      </div>
      {err && <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>}
      {Object.values(progress).length > 0 && (
        <div className="card">
          <h3>Progress</h3>
          {Object.values(progress).map((p) => (
            <div key={p.label} style={{ marginBottom: 6 }}>
              <code>{p.label}</code> — {fmtBytes(p.bytes)}
              {p.total ? ` / ${fmtBytes(p.total)} (${pct(p.bytes, p.total)}%)` : ""}
              {p.done ? " ✓" : ""}
            </div>
          ))}
        </div>
      )}
    </>
  );
}

function allPresent(s: ModelsStatus): boolean {
  return (
    s.whisper_live_present &&
    s.whisper_finalize_present &&
    s.sherpa_segmentation_present &&
    s.sherpa_embedding_present
  );
}
function fmtBytes(n: number): string {
  if (n > 1e9) return `${(n / 1e9).toFixed(2)} GB`;
  if (n > 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n > 1e3) return `${(n / 1e3).toFixed(0)} KB`;
  return `${n} B`;
}
function pct(n: number, total: number): number {
  return Math.round((n / total) * 100);
}
