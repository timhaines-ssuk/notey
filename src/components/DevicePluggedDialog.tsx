import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
  api,
  ImportProgress,
  ImportResult,
  PluggedDevice,
  SyncSummary,
} from "../lib/tauri-api";

type Phase = "preview" | "syncing" | "done";

export default function DevicePluggedDialog() {
  const [device, setDevice] = useState<PluggedDevice | null>(null);
  const [phase, setPhase] = useState<Phase>("preview");
  const [progress, setProgress] = useState<Record<string, ImportProgress>>({});
  const [summary, setSummary] = useState<SyncSummary | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [wantWipe, setWantWipe] = useState(false);

  useEffect(() => {
    const un = listen<PluggedDevice>("device-plugged", (e) => {
      setDevice(e.payload);
      setPhase("preview");
      setProgress({});
      setSummary(null);
      setErr(null);
      setWantWipe(false);
    });
    return () => { un.then((u) => u()); };
  }, []);

  useEffect(() => {
    const un = listen<ImportProgress>("import-progress", (e) => {
      setProgress((p) => ({ ...p, [e.payload.source]: e.payload }));
    });
    return () => { un.then((u) => u()); };
  }, []);

  async function run(wipe: boolean) {
    if (!device) return;
    setPhase("syncing");
    setWantWipe(wipe);
    setErr(null);
    try {
      const s = await api.syncDevice(device.mount, device.audio_files, wipe);
      setSummary(s);
      setPhase("done");
    } catch (e) {
      setErr(String(e));
      setPhase("preview");
    }
  }

  if (!device) return null;

  return (
    <Overlay>
      <div className="card" style={{ minWidth: 520, maxWidth: 760 }}>
        <h3>
          USB recorder
          {device.label ? <> · <strong>{device.label}</strong></> : null}
          <span style={{ color: "#8b8f99", fontSize: 12, marginLeft: 8, fontWeight: "normal" }}>
            <code>{device.mount}</code>
          </span>
        </h3>

        {phase === "preview" && (
          <Preview
            device={device}
            err={err}
            onSync={() => run(false)}
            onSyncWipe={() => run(true)}
            onCancel={() => setDevice(null)}
          />
        )}

        {phase === "syncing" && (
          <Syncing device={device} progress={progress} wipe={wantWipe} />
        )}

        {phase === "done" && summary && (
          <Done
            summary={summary}
            wanted_wipe={wantWipe}
            onClose={() => setDevice(null)}
          />
        )}
      </div>
    </Overlay>
  );
}

function Preview({
  device,
  err,
  onSync,
  onSyncWipe,
  onCancel,
}: {
  device: PluggedDevice;
  err: string | null;
  onSync: () => void;
  onSyncWipe: () => void;
  onCancel: () => void;
}) {
  return (
    <>
      <p>Found <strong>{device.audio_files.length}</strong> audio file(s):</p>
      <ul style={{ maxHeight: 180, overflow: "auto", fontSize: 12, fontFamily: "monospace" }}>
        {device.audio_files.slice(0, 50).map((f) => (
          <li key={f}>{shortPath(f, device.mount)}</li>
        ))}
        {device.audio_files.length > 50 && <li>…and {device.audio_files.length - 50} more</li>}
      </ul>
      <p style={{ color: "#8b8f99", fontSize: 12 }}>
        Each file is copied, SHA-256 hashed on both sides, and only files whose source &amp; dest
        hashes match are treated as verified. If you choose <em>Sync &amp; wipe</em>, only
        verified files are deleted from the device — failed ones stay put.
      </p>
      {err && <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>}
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 12 }}>
        <button onClick={onCancel}>Cancel</button>
        <button className="primary" onClick={onSync}>Sync</button>
        <button
          onClick={() => {
            if (confirm(`Verify-and-then-delete ${device.audio_files.length} files from the device? Only files whose copies are verified will be deleted.`)) {
              onSyncWipe();
            }
          }}
          style={{ background: "#7a3838", color: "white", border: 0, padding: "8px 14px", borderRadius: 6 }}
        >
          Sync &amp; wipe device
        </button>
      </div>
    </>
  );
}

function Syncing({
  device,
  progress,
  wipe,
}: {
  device: PluggedDevice;
  progress: Record<string, ImportProgress>;
  wipe: boolean;
}) {
  const done = Object.values(progress).filter((p) => p.stage !== "copying").length;
  const verified = Object.values(progress).filter((p) => p.stage === "verified").length;
  const failed = Object.values(progress).filter((p) => p.stage === "failed").length;

  return (
    <>
      <p>
        Copying &amp; verifying… <strong>{done}</strong> of {device.audio_files.length}
        {" "}({verified} verified, {failed} failed){wipe ? " — wipe pending verification" : ""}
      </p>
      <div style={{ maxHeight: 240, overflow: "auto" }}>
        {device.audio_files.map((f) => {
          const p = progress[f];
          const stage = p?.stage ?? "queued";
          return (
            <div key={f} style={{ display: "flex", gap: 8, alignItems: "center", padding: "4px 0", fontSize: 12 }}>
              <span style={{ width: 80 }}>
                <span className={`status-pill ${stage === "verified" ? "complete" : stage === "failed" ? "failed" : ""}`}>
                  {stage}
                </span>
              </span>
              <span style={{ fontFamily: "monospace" }}>{shortPath(f, device.mount)}</span>
              {p?.error && <span style={{ color: "#e8b1b1" }}>— {p.error}</span>}
            </div>
          );
        })}
      </div>
    </>
  );
}

function Done({
  summary,
  wanted_wipe,
  onClose,
}: {
  summary: SyncSummary;
  wanted_wipe: boolean;
  onClose: () => void;
}) {
  return (
    <>
      <p>
        <strong>{summary.verified_count}</strong> file(s) imported &amp; verified.
        {summary.failed_count > 0 && (
          <> <span style={{ color: "#e8b1b1" }}>{summary.failed_count} failed.</span></>
        )}
      </p>
      {wanted_wipe && (
        <p>
          {summary.wipe_error ? (
            <span style={{ color: "#e8b1b1" }}>Wipe failed: {summary.wipe_error}</span>
          ) : (
            <>Deleted <strong>{summary.wiped_count}</strong> verified file(s) from device.</>
          )}
        </p>
      )}
      {summary.failed_count > 0 && (
        <details style={{ marginTop: 8 }}>
          <summary style={{ cursor: "pointer" }}>Failed file details</summary>
          <div style={{ maxHeight: 200, overflow: "auto", fontFamily: "monospace", fontSize: 12, marginTop: 6 }}>
            {summary.results.filter((r: ImportResult) => !r.verified).map((r) => (
              <div key={r.source_path} style={{ marginBottom: 6 }}>
                <div>{r.source_path}</div>
                <div style={{ color: "#e8b1b1" }}>{r.error ?? "unknown error"}</div>
              </div>
            ))}
          </div>
        </details>
      )}
      <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 12 }}>
        <button className="primary" onClick={onClose}>Done</button>
      </div>
    </>
  );
}

function Overlay({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      position: "fixed", inset: 0, background: "rgba(0,0,0,0.55)",
      display: "grid", placeItems: "center", zIndex: 15,
    }}>
      {children}
    </div>
  );
}

function shortPath(full: string, mount: string) {
  if (full.startsWith(mount)) return full.slice(mount.length);
  return full;
}

export async function manualImport(): Promise<string[]> {
  const sel = await open({
    multiple: true,
    filters: [{ name: "Audio", extensions: ["wav"] }],
  });
  if (!sel) return [];
  const arr = Array.isArray(sel) ? sel : [sel];
  return arr.map((s) => (typeof s === "string" ? s : (s as any).path));
}
