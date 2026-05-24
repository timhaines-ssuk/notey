import { useEffect, useState } from "react";
import { api, UnnamedCluster } from "../lib/tauri-api";

export default function SpeakerNamingDialog({
  recordingId,
  onClose,
}: {
  recordingId: number;
  onClose: () => void;
}) {
  const [clusters, setClusters] = useState<UnnamedCluster[]>([]);
  const [idx, setIdx] = useState(0);
  const [name, setName] = useState("");

  useEffect(() => {
    api.getUnnamedClusters(recordingId).then(setClusters);
  }, [recordingId]);

  if (clusters.length === 0) {
    return (
      <Overlay onClose={onClose}>
        <div className="card">No unnamed speakers — done.</div>
      </Overlay>
    );
  }
  if (idx >= clusters.length) {
    return (
      <Overlay onClose={onClose}>
        <div className="card">All speakers named.</div>
        <button className="primary" onClick={onClose}>Close</button>
      </Overlay>
    );
  }

  const c = clusters[idx];

  async function confirmNew() {
    if (!name.trim()) return;
    await api.confirmSpeaker(recordingId, c.cluster_id, { kind: "new", name });
    setName("");
    setIdx(idx + 1);
  }
  async function confirmExisting(speakerId: number) {
    await api.confirmSpeaker(recordingId, c.cluster_id, { kind: "existing", speakerId });
    setIdx(idx + 1);
  }
  async function confirmNoise() {
    await api.confirmSpeaker(recordingId, c.cluster_id, { kind: "noise" });
    setIdx(idx + 1);
  }

  return (
    <Overlay onClose={onClose}>
      <div className="card" style={{ minWidth: 480 }}>
        <h3>Who is Speaker {c.cluster_id}? ({idx + 1}/{clusters.length})</h3>
        {c.snippets.map((s, i) => (
          <div key={i} style={{ marginBottom: 6 }}>
            <span style={{ color: "#8b8f99", fontFamily: "monospace", marginRight: 8 }}>
              {s.start.toFixed(1)}–{s.end.toFixed(1)}s
            </span>
            <em>"{s.text}"</em>
          </div>
        ))}
        {c.suggestions.length > 0 && (
          <>
            <h4>Suggestions</h4>
            {c.suggestions.map((sg) => (
              <div key={sg.speaker_id} style={{ marginBottom: 4 }}>
                <button onClick={() => confirmExisting(sg.speaker_id)}>
                  {sg.name} — {(sg.similarity * 100).toFixed(0)}% match
                </button>
              </div>
            ))}
          </>
        )}
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <input
            type="text"
            placeholder="New name…"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && confirmNew()}
            style={{ flex: 1 }}
          />
          <button className="primary" onClick={confirmNew}>Save</button>
          <button onClick={confirmNoise}>Not a person</button>
        </div>
      </div>
    </Overlay>
  );
}

function Overlay({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.55)",
        display: "grid",
        placeItems: "center",
        zIndex: 10,
      }}
      onClick={onClose}
    >
      <div onClick={(e) => e.stopPropagation()}>{children}</div>
    </div>
  );
}
