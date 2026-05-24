import { useEffect, useState } from "react";
import { api } from "../lib/tauri-api";

interface AudioSession {
  pid: number;
  display_name: string;
  process_name: string;
}

export default function SettingsAudio() {
  const [devices, setDevices] = useState<{
    inputs: string[];
    outputs: string[];
    default_input: string | null;
    default_output: string | null;
  } | null>(null);
  const [sessions, setSessions] = useState<AudioSession[]>([]);
  const [settings, setSettings] = useState<Record<string, string>>({});
  const [err, setErr] = useState<string | null>(null);

  async function refreshAll() {
    try {
      const [d, s, st] = await Promise.all([
        api.listAudioDevices(),
        api.listAudioSessions(),
        api.getSettings(),
      ]);
      setDevices(d);
      setSessions(s);
      setSettings(st);
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => {
    refreshAll();
  }, []);

  async function update(key: string, value: string) {
    setSettings({ ...settings, [key]: value });
    try {
      await api.setSetting(key, value);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function refreshProcesses() {
    try {
      setSessions(await api.listAudioSessions());
    } catch (e) {
      setErr(String(e));
    }
  }

  if (err && !devices) return <div className="card" style={{ color: "#e8b1b1" }}>{err}</div>;
  if (!devices) return <div>Loading audio devices…</div>;

  const mic = settings.device_mic ?? "";
  const source = settings.loopback_source ?? "default";
  const loopDevice = settings.device_loopback ?? "";

  return (
    <>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <h2>Audio devices</h2>
        <button onClick={refreshAll}>Refresh all</button>
      </div>

      <div className="card">
        <h3>Microphone (left channel)</h3>
        <p style={{ color: "#8b8f99", marginTop: 0 }}>
          Pick which input device captures <strong>your</strong> voice.
        </p>
        <select
          value={mic}
          onChange={(e) => update("device_mic", e.target.value)}
          style={{ width: "100%" }}
        >
          <option value="">Default ({devices.default_input ?? "none detected"})</option>
          {devices.inputs.map((d) => (
            <option key={d} value={d}>{d}</option>
          ))}
        </select>
      </div>

      <div className="card">
        <h3>Loopback source (right channel)</h3>
        <p style={{ color: "#8b8f99", marginTop: 0 }}>
          Choose where the "other side" audio comes from. Two modes available:
        </p>
        <div style={{ marginTop: 8 }}>
          <strong>1. Output device (whole-system loopback)</strong>
          <p style={{ color: "#8b8f99", margin: "4px 0 8px 0" }}>
            Captures everything playing through the chosen output device — Discord, Spotify,
            notifications, browser, the lot.
          </p>
          <SourceRow
            id="loopback-default"
            checked={source === "default"}
            onSelect={() => update("loopback_source", "default")}
            label={
              <>
                Default output device{" "}
                <span style={{ color: "#8b8f99" }}>({devices.default_output ?? "none detected"})</span>
              </>
            }
          />
          {devices.outputs
            .filter((d) => d !== devices.default_output)
            .map((d) => (
              <SourceRow
                key={d}
                id={`loopback-dev-${d}`}
                checked={source === "default" && loopDevice === d}
                onSelect={async () => {
                  await update("loopback_source", "default");
                  await update("device_loopback", d);
                }}
                label={<>Output device · <code>{d}</code></>}
              />
            ))}
        </div>

        <div style={{ marginTop: 16, display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <strong>2. Application audio (per-process loopback)</strong>
          <button onClick={refreshProcesses} style={{ fontSize: 12 }}>Refresh apps</button>
        </div>
        <p style={{ color: "#8b8f99", margin: "4px 0 8px 0" }}>
          Records only what the selected app is playing — same mechanism OBS uses. Each app must
          be running and playing audio for it to appear in this list.
        </p>
        <SourceRow
          id="loopback-discord"
          checked={source === "discord"}
          onSelect={() => update("loopback_source", "discord")}
          label={
            <>
              <strong>Discord</strong>{" "}
              <span style={{ color: "#8b8f99" }}>
                (auto-detects Discord.exe / DiscordCanary.exe / DiscordPTB.exe at capture start)
              </span>
            </>
          }
        />
        <SourceRow
          id="loopback-teams"
          checked={source === "teams"}
          onSelect={() => update("loopback_source", "teams")}
          label={
            <>
              <strong>Microsoft Teams</strong>{" "}
              <span style={{ color: "#8b8f99" }}>(auto-detects ms-teams.exe / Teams.exe)</span>
            </>
          }
        />
        {sessions.length === 0 && (
          <p style={{ color: "#8b8f99", fontSize: 12, fontStyle: "italic", marginTop: 8 }}>
            No other audio-producing apps detected right now. Start playing audio in an app and click Refresh.
          </p>
        )}
        {sessions.map((s) => (
          <SourceRow
            key={s.pid}
            id={`loopback-pid-${s.pid}`}
            checked={source === `pid:${s.pid}`}
            onSelect={() => update("loopback_source", `pid:${s.pid}`)}
            label={
              <>
                {s.display_name}{" "}
                <span style={{ color: "#8b8f99", fontSize: 11 }}>
                  · {s.process_name} (pid {s.pid})
                </span>
              </>
            }
          />
        ))}
      </div>

      <div className="card" style={{ color: "#8b8f99" }}>
        Tip: after picking a source, click <em>Start call capture</em> on the Recordings page and
        watch the two meters — both should move when you talk and when audio plays.
      </div>
    </>
  );
}

function SourceRow({
  id,
  checked,
  onSelect,
  label,
}: {
  id: string;
  checked: boolean;
  onSelect: () => void;
  label: React.ReactNode;
}) {
  return (
    <label
      htmlFor={id}
      style={{
        display: "flex",
        gap: 8,
        padding: "6px 10px",
        borderRadius: 6,
        background: checked ? "#2a2c31" : "transparent",
        cursor: "pointer",
        alignItems: "center",
      }}
    >
      <input
        type="radio"
        id={id}
        name="loopback-source"
        checked={checked}
        onChange={onSelect}
      />
      <span>{label}</span>
    </label>
  );
}
