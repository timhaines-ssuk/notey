import { useEffect, useState } from "react";
import { api } from "../lib/tauri-api";

const WHISPER_MODELS = [
  "tiny.en", "tiny", "base.en", "base", "small.en", "small",
  "medium.en", "medium", "large-v3", "large-v3-turbo",
];
const LLM_MODELS = ["llama3.2:3b", "qwen2.5:3b", "qwen2.5:7b", "qwen2.5:14b", "llama3.1:8b"];
const BACKENDS = ["cpu", "cuda", "directml"];

export default function SettingsModels() {
  const [settings, setSettings] = useState<Record<string, string>>({});

  useEffect(() => { api.getSettings().then(setSettings); }, []);

  function update(key: string, value: string) {
    setSettings({ ...settings, [key]: value });
    api.setSetting(key, value);
  }

  const liveCfg = parseCfg(settings["transcribe_live"]);
  const finalCfg = parseCfg(settings["transcribe_finalize"]);
  const summCfg = parseCfg(settings["summarize"]);

  return (
    <>
      <h2>Models</h2>
      <div className="card">
        <h3>Transcription — live</h3>
        <ModelRow
          model={liveCfg.model} models={WHISPER_MODELS}
          backend={liveCfg.backend}
          onModel={(m) => update("transcribe_live", stringifyCfg({ ...liveCfg, model: m }))}
          onBackend={(b) => update("transcribe_live", stringifyCfg({ ...liveCfg, backend: b }))}
        />
      </div>
      <div className="card">
        <h3>Transcription — finalize</h3>
        <ModelRow
          model={finalCfg.model} models={WHISPER_MODELS}
          backend={finalCfg.backend}
          onModel={(m) => update("transcribe_finalize", stringifyCfg({ ...finalCfg, model: m }))}
          onBackend={(b) => update("transcribe_finalize", stringifyCfg({ ...finalCfg, backend: b }))}
        />
      </div>
      <div className="card">
        <h3>Summarization</h3>
        <ModelRow
          model={summCfg.model} models={LLM_MODELS}
          backend={summCfg.backend}
          onModel={(m) => update("summarize", stringifyCfg({ ...summCfg, model: m }))}
          onBackend={(b) => update("summarize", stringifyCfg({ ...summCfg, backend: b }))}
        />
      </div>
    </>
  );
}

function ModelRow({
  model, models, backend, onModel, onBackend,
}: {
  model: string; models: string[]; backend: string;
  onModel: (s: string) => void; onBackend: (s: string) => void;
}) {
  return (
    <div style={{ display: "flex", gap: 12 }}>
      <label>
        Model{" "}
        <select value={model} onChange={(e) => onModel(e.target.value)}>
          {models.map((m) => <option key={m}>{m}</option>)}
        </select>
      </label>
      <label>
        Backend{" "}
        <select value={backend} onChange={(e) => onBackend(e.target.value)}>
          {BACKENDS.map((b) => <option key={b}>{b}</option>)}
        </select>
      </label>
    </div>
  );
}

interface Cfg { model: string; backend: string; device_index: number }
function parseCfg(s?: string): Cfg {
  try { return { model: "", backend: "cuda", device_index: 0, ...JSON.parse(s ?? "{}") }; }
  catch { return { model: "", backend: "cuda", device_index: 0 }; }
}
function stringifyCfg(c: Cfg) { return JSON.stringify(c); }
