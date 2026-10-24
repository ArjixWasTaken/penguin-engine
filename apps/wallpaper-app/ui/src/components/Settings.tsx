// Settings: the daemon's (frame rate, sound, fill, layer, input), which
// displays "Apply" changes, and starting with the session.
import { Gauge, Info, Monitor, Settings as Gear, Volume2, X } from "lucide-preact";
import { useEffect, useMemo, useState } from "preact/hooks";
import { api, thumbUrl, type Fill, type Settings as DaemonSettings } from "../api";
import { Throttled } from "../props";
import { request, toast, useApp, type SettingsTab } from "../store";
import { Segments, Toggle } from "./ui";
import { VERSION } from "./TitleBar";

const TABS: [SettingsTab, typeof Gear][] = [
  ["General", Gear],
  ["Performance", Gauge],
  ["Displays", Monitor],
  ["Audio", Volume2],
  ["About", Info],
];

function patch(p: Partial<DaemonSettings>) {
  return request({ cmd: "settings", ...p });
}

export function SettingsDialog() {
  const tab = useApp((s) => s.settingsTab);
  const settings = useApp((s) => s.status?.settings);
  const close = () => useApp.setState({ modal: null });
  return (
    <div class="backdrop" onClick={close}>
      <div class="modal settings" onClick={(e) => e.stopPropagation()}>
        <div class="modal-nav">
          <div class="title">Settings</div>
          {TABS.map(([label, Icon]) => (
            <button class={tab === label ? "on" : ""} onClick={() => useApp.setState({ settingsTab: label })}>
              <Icon size={15} />
              {label}
            </button>
          ))}
        </div>
        <div class="modal-main">
          <div class="modal-head">
            <div class="title">{tab}</div>
            <button class="modal-close" onClick={close}>
              <X size={15} />
            </button>
          </div>
          <div class="modal-body">
            {!settings && tab !== "About" && tab !== "General" ? (
              <div class="muted">The wallpaper daemon isn't running.</div>
            ) : (
              <>
                {tab === "General" && <General settings={settings} />}
                {tab === "Performance" && settings && <Performance settings={settings} />}
                {tab === "Displays" && settings && <Displays settings={settings} />}
                {tab === "Audio" && settings && <Audio settings={settings} />}
                {tab === "About" && <About />}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function Setting({ label, hint, children }: { label: string; hint?: string; children: preact.ComponentChildren }) {
  return (
    <div class="setting">
      <div>
        <div>{label}</div>
        {hint && <div class="hint">{hint}</div>}
      </div>
      {children}
    </div>
  );
}

function General({ settings }: { settings?: DaemonSettings }) {
  const [autostart, setAutostart] = useState<boolean | null>(null);
  useEffect(() => {
    api.autostart().then(setAutostart, () => setAutostart(null));
  }, []);
  const flip = async (on: boolean) => {
    try {
      await api.setAutostart(on);
      setAutostart(on);
    } catch (e) {
      toast(`Couldn't change it: ${e}`, true);
    }
  };
  return (
    <>
      <Setting label="Start with session" hint="Show wallpapers when you log in (the daemon's systemd units)">
        <Toggle on={!!autostart} disabled={autostart === null} onChange={flip} />
      </Setting>
      {settings && (
        <>
          <Setting label="Let clicks through" hint="Clicks reach the desktop instead of the wallpaper">
            <Toggle on={settings.passthrough} onChange={(passthrough) => patch({ passthrough })} />
          </Setting>
          <div class="setting-col">
            <div>Layer</div>
            <Segments
              value={settings.layer}
              options={["background", "bottom"] as const}
              labels={{ background: "Background", bottom: "Above desktop icons" }}
              onChange={(layer) => patch({ layer })}
            />
          </div>
        </>
      )}
    </>
  );
}

function Performance({ settings }: { settings: DaemonSettings }) {
  const [fps, setFps] = useState<number | null>(null);
  const throttle = useMemo(() => new Throttled((v) => patch({ fps: v })), []);
  useEffect(() => setFps(null), [settings.fps]);
  const v = fps ?? settings.fps;
  return (
    <label class="setting-col">
      <div class="head">
        <span>Frame rate limit</span>
        <span class="mono muted">{v === 0 ? "display rate" : `${v} fps`}</span>
      </div>
      <input
        type="range"
        min={0}
        max={240}
        step={5}
        value={v}
        onInput={(e) => {
          const n = +(e.target as HTMLInputElement).value;
          setFps(n);
          throttle.set(n);
        }}
      />
      <div class="hint">0 follows the display's refresh rate.</div>
    </label>
  );
}

function Displays({ settings }: { settings: DaemonSettings }) {
  const outputs = useApp((s) => s.status?.outputs ?? []);
  const target = useApp((s) => s.target);
  const items = useApp((s) => s.items);
  const byPath = useMemo(() => new Map(items.map((i) => [i.path, i])), [items]);
  const biggest = Math.max(1, ...outputs.map((o) => Math.max(o.width, o.height)));
  const fill = target ? outputs.find((o) => o.name === target)?.fill ?? settings.fill : settings.fill;
  const setFill = (f: Fill) => (target ? request({ cmd: "fill", output: target, fill: f }) : patch({ fill: f }));
  return (
    <>
      <div class="displays">
        {outputs.map((o, n) => {
          const scale = 200 / biggest;
          const item = o.wallpaper ? byPath.get(o.wallpaper.path) : undefined;
          return (
            <button class={"display" + (target === o.name ? " on" : "")} onClick={() => useApp.setState({ target: o.name })}>
              <div class="screen" style={{ width: Math.max(60, o.width * scale), height: Math.max(40, o.height * scale) }}>
                {item && <img src={thumbUrl(item.id)} alt="" />}
                <b>{n + 1}</b>
              </div>
              <div class="label">{`${o.name} · ${o.width} x ${o.height}`}{o.scale !== 1 ? ` @${o.scale}x` : ""}</div>
              <div class="wall">{o.wallpaper?.title ?? "No wallpaper"}</div>
            </button>
          );
        })}
      </div>
      <div class="setting-col">
        <div>Apply wallpapers to</div>
        <Segments
          value={target === null ? "all" : "one"}
          options={["one", "all"] as const}
          labels={{ one: target ? `This display (${target})` : "One display", all: "All displays" }}
          onChange={(v) => useApp.setState({ target: v === "all" ? null : target ?? outputs[0]?.name ?? null })}
        />
      </div>
      <div class="setting-col">
        <div>Fit {target ? `on ${target}` : "on every display"}</div>
        <Segments
          value={fill}
          options={["auto", "crop", "fit", "stretch"] as const}
          labels={{ auto: "Auto", crop: "Crop", fit: "Fit", stretch: "Stretch" }}
          onChange={setFill}
        />
        <div class="hint">Auto crops, but fits a portrait wallpaper on a landscape display (and the reverse).</div>
      </div>
    </>
  );
}

function Audio({ settings }: { settings: DaemonSettings }) {
  const [volume, setVolume] = useState<number | null>(null);
  const throttle = useMemo(() => new Throttled((v) => patch({ volume: v })), []);
  useEffect(() => setVolume(null), [settings.volume]);
  const v = volume ?? settings.volume;
  return (
    <>
      <Setting label="Sound and audio visualizers" hint="Wallpapers play sound and react to what your system plays">
        <Toggle on={settings.audio} onChange={(audio) => patch({ audio })} />
      </Setting>
      <label class="setting-col">
        <div class="head">
          <span>Master volume</span>
          <span class="mono muted">{v}%</span>
        </div>
        <input
          type="range"
          min={0}
          max={100}
          value={v}
          onInput={(e) => {
            const n = +(e.target as HTMLInputElement).value;
            setVolume(n);
            throttle.set(n);
          }}
        />
        <div class="hint">Each wallpaper's own volume (in its properties) is a share of this.</div>
      </label>
    </>
  );
}

function About() {
  const daemon = useApp((s) => s.daemonVersion);
  const features = useApp((s) => s.daemonFeatures);
  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
        <div class="logo big" />
        <div>
          <div style={{ fontSize: 18, fontWeight: 600 }}>Penguin Engine</div>
          <div class="mono muted" style={{ fontSize: 12 }}>
            Version {VERSION}
            {daemon ? ` · daemon ${daemon}` : ""}
            {features.length ? ` (${features.join(", ")})` : ""}
          </div>
        </div>
      </div>
      <div style={{ color: "var(--text-2)", lineHeight: 1.6, maxWidth: 440, textWrap: "pretty" }}>
        Animated wallpapers for your desktop: Wallpaper Engine's scenes, videos and web wallpapers, on every display.
      </div>
    </>
  );
}
