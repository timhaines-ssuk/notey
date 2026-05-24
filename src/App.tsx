import { useEffect, useState } from "react";
import RecordingsList from "./components/RecordingsList";
import TranscriptViewer from "./components/TranscriptViewer";
import SettingsHardware from "./components/SettingsHardware";
import SettingsModels from "./components/SettingsModels";
import SettingsAudio from "./components/SettingsAudio";
import ModelDownloader from "./components/ModelDownloader";
import DevicePluggedDialog from "./components/DevicePluggedDialog";
import { api } from "./lib/tauri-api";

type View =
  | { kind: "models" }
  | { kind: "recordings" }
  | { kind: "transcript"; recordingId: number }
  | { kind: "settings-hardware" }
  | { kind: "settings-audio" }
  | { kind: "settings-models" };

export default function App() {
  const [view, setView] = useState<View | null>(null);

  useEffect(() => {
    api
      .modelsStatus()
      .then((s) => {
        const ok =
          s.whisper_live_present &&
          s.whisper_finalize_present &&
          s.sherpa_segmentation_present &&
          s.sherpa_embedding_present;
        setView(ok ? { kind: "recordings" } : { kind: "models" });
      })
      .catch(() => setView({ kind: "recordings" }));
  }, []);

  if (!view) return null;

  return (
    <>
      <nav className="sidebar">
        <h1>Notetaker</h1>
        <button
          className={view.kind === "recordings" || view.kind === "transcript" ? "active" : ""}
          onClick={() => setView({ kind: "recordings" })}
        >
          Recordings
        </button>
        <button
          className={view.kind === "settings-audio" ? "active" : ""}
          onClick={() => setView({ kind: "settings-audio" })}
        >
          Settings · Audio
        </button>
        <button
          className={view.kind === "settings-hardware" ? "active" : ""}
          onClick={() => setView({ kind: "settings-hardware" })}
        >
          Settings · Hardware
        </button>
        <button
          className={view.kind === "settings-models" ? "active" : ""}
          onClick={() => setView({ kind: "settings-models" })}
        >
          Settings · Models
        </button>
        <button
          className={view.kind === "models" ? "active" : ""}
          onClick={() => setView({ kind: "models" })}
        >
          Model downloads
        </button>
      </nav>
      <main className="view">
        {view.kind === "models" && (
          <ModelDownloader onDone={() => setView({ kind: "recordings" })} />
        )}
        {view.kind === "recordings" && (
          <RecordingsList onOpen={(id) => setView({ kind: "transcript", recordingId: id })} />
        )}
        {view.kind === "transcript" && (
          <TranscriptViewer
            recordingId={view.recordingId}
            onBack={() => setView({ kind: "recordings" })}
          />
        )}
        {view.kind === "settings-audio" && <SettingsAudio />}
        {view.kind === "settings-hardware" && <SettingsHardware />}
        {view.kind === "settings-models" && <SettingsModels />}
      </main>
      <DevicePluggedDialog />
    </>
  );
}
