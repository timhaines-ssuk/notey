import { useEffect, useState } from "react";
import { HardwareProfile, getHardwareCached, reDetectHardware } from "../lib/tauri-api";

export default function SettingsHardware() {
  const [hw, setHw] = useState<HardwareProfile | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    getHardwareCached().then(setHw).catch((e) => setErr(String(e)));
  }, []);

  async function redetect() {
    setHw(null);
    try {
      setHw(await reDetectHardware());
    } catch (e) {
      setErr(String(e));
    }
  }

  if (err) return <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>;
  if (!hw) return <div>Detecting…</div>;

  return (
    <>
      <h2>Hardware</h2>
      <div className="card">
        <h3>System</h3>
        <table className="kv">
          <tbody>
            <tr><td>CPU cores</td><td>{hw.cpu_cores}</td></tr>
            <tr><td>RAM</td><td>{hw.total_ram_gb} GB</td></tr>
            <tr><td>Recommended backend</td><td>{hw.recommended_backend}</td></tr>
          </tbody>
        </table>
      </div>
      <div className="card">
        <h3>GPUs</h3>
        {hw.gpus.length === 0 && <em>None detected.</em>}
        {hw.gpus.map((g, i) => (
          <div key={i} className="hw-gpu">
            <div className="name">{g.name}</div>
            <div className="detail">
              {g.vendor} · {g.vram_mb ? `${g.vram_mb} MB VRAM` : "VRAM unknown"} ·
              {g.cuda_capable ? " CUDA" : ""} {g.directml_capable ? " DirectML" : ""}
            </div>
          </div>
        ))}
      </div>
      <div className="card">
        <h3>Recommended models</h3>
        <table className="kv">
          <tbody>
            <tr><td>Whisper (live)</td><td>{hw.recommended_whisper_live}</td></tr>
            <tr><td>Whisper (finalize)</td><td>{hw.recommended_whisper_finalize}</td></tr>
            <tr><td>Summarization LLM</td><td>{hw.recommended_llm}</td></tr>
          </tbody>
        </table>
      </div>
      <button className="primary" onClick={redetect}>Re-detect hardware</button>
    </>
  );
}
