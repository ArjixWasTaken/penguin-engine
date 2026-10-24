// A stand-in backend with made-up wallpapers, used when the page runs in
// a plain browser (`pnpm dev`, opened directly): for working on the page
// without the app or a daemon.
import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Item, Property, Status, Values } from "./api";

const NAMES = ["Glacier Drift", "Neon Rainfall", "Aurora over Fjord", "Midnight Tram", "Koi Pond Loop", "Synth Sunset", "Lo-fi Study Room", "Orbital Clock", "Desert Mirage", "Cherry Blossom Rain", "Deep Sea Lantern", "City Lights", "Pixel Forest", "Rainy Window Café", "Saturn Rings", "Ink Wash Mountains", "Holographic Waves", "Audio Spectrum Bars", "Ice Floe Penguins", "Starfield Parallax", "Paper Lanterns", "Retro Arcade", "Foggy Lighthouse", "Crystal Cavern", "Night Market", "Moonlit Shrine", "Vapor Grid", "Autumn Railway", "Bioluminescent Bay", "Clockwork Heart"];
const TAGS = ["Abstract", "Anime", "Landscape", "Nature", "Pixel art", "Relaxing", "Sci-Fi", "Space", "Music"];
const KINDS = ["scene", "scene", "scene", "video", "web"] as const;
const rnd = (i: number, s: number) => {
  const x = Math.sin(i * 12.9898 + s * 78.233) * 43758.5453;
  return x - Math.floor(x);
};

const items: Item[] = Array.from({ length: 240 }, (_, i) => {
  const name = NAMES[i % NAMES.length] + (i >= NAMES.length ? ` ${Math.floor(i / NAMES.length) + 1}` : "");
  return {
    id: String(1000 + i),
    path: `/mock/${1000 + i}`,
    title: name,
    description: null,
    kind: KINDS[Math.floor(rnd(i, 1) * KINDS.length)],
    tags: [TAGS[Math.floor(rnd(i, 2) * TAGS.length)]],
    rating: rnd(i, 3) < 0.85 ? "everyone" : "questionable",
    preview: "preview.gif",
    size: Math.round(rnd(i, 4) * 300e6),
    updated: 1.7e9 + Math.round(rnd(i, 5) * 6e7),
    installed: 1.7e9 + Math.round(rnd(i, 6) * 6e7),
    workshop_id: String(1000 + i),
    source: i % 13 === 0 ? "projects" : "workshop",
    audio_reactive: rnd(i, 7) > 0.7,
    has_properties: true,
    dependency: null,
  };
});

const props: Property[] = [
  { name: "schemecolor", type: "color", label: "Scheme color", text: null, value: "0.31 0.76 0.91", default: "0.31 0.76 0.91" },
  { name: "speed", type: "slider", label: "Speed", text: null, value: 1, default: 1, min: 0, max: 3, step: 0.1 },
  { name: "rain", type: "bool", label: "Rain", text: null, value: true, default: true },
  { name: "intensity", type: "slider", label: "Rain intensity", text: null, value: 50, default: 50, min: 0, max: 100, step: 1, condition: "rain.value" },
  { name: "mode", type: "combo", label: "Clock", text: null, value: "24", default: "24", options: [{ label: "24 hours", value: "24" }, { label: "12 hours", value: "12" }] },
];

const status: Status = {
  settings: {
    fps: 60, audio: true, volume: 80, fill: "auto", layer: "background", passthrough: false, particles: 100,
    rules: [
      { when: { if: "window", state: "fullscreen" }, playback: "pause" },
      { when: { if: "audio", app: "spotify" }, playback: "mute" },
      { when: { if: "battery" }, fps: 30, particles: 50 },
      { enabled: false, when: { if: "idle", seconds: 600 }, playback: "stop" },
    ],
  },
  outputs: [
    { name: "DP-1", width: 2560, height: 1440, scale: 1, wallpaper: { path: items[4].path, workshop_id: items[4].id, title: items[4].title, type: "scene", sound: true, volume: 70 }, paused: true, fps: 0, fill: "auto", loading: null, limits: { playback: "pause", because: ["org.mozilla.firefox is fullscreen"] } },
    { name: "HDMI-A-1", width: 1080, height: 1920, scale: 1, wallpaper: { path: items[9].path, workshop_id: items[9].id, title: items[9].title, type: "video" }, paused: false, fps: 30, fill: "auto", loading: null },
  ],
};

function art(id: string): string {
  const i = +id;
  const h = Math.round(rnd(i, 8) * 360), h2 = (h + 60 + Math.round(rnd(i, 9) * 120)) % 360;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${h},55%,45%)"/><stop offset="1" stop-color="hsl(${h2},60%,14%)"/></linearGradient><radialGradient id="r" cx="${0.2 + rnd(i, 10) * 0.6}" cy="${0.2 + rnd(i, 11) * 0.5}" r=".4"><stop offset="0" stop-color="hsl(${h},90%,85%)"/><stop offset="1" stop-color="hsl(${h},90%,85%)" stop-opacity="0"/></radialGradient></defs><rect width="10" height="10" fill="url(#g)"/><rect width="10" height="10" fill="url(#r)"/></svg>`;
  return "data:image/svg+xml," + encodeURIComponent(svg);
}

export function mockPicture(url: string): string {
  return art(decodeURIComponent(url.split("/").pop() ?? "0"));
}

export function installMock() {
  (window as unknown as { __PENGUIN_MOCK__: boolean }).__PENGUIN_MOCK__ = true;
  mockWindows("main");
  let ui: unknown = null;
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "library":
          return items;
        case "daemon": {
          const r = a.request as { cmd: string };
          if (r.cmd === "status") return { ok: true, status };
          if (r.cmd === "version") return { ok: true, version: { protocol: 2, daemon: "0.1.0", features: [] } };
          if (r.cmd === "properties") return { ok: true, properties: [] };
          if (r.cmd === "signals")
            return { ok: true, signals: { windows: [{ app: "org.mozilla.firefox", process: "firefox", title: "Video", output: "DP-1", fullscreen: true, focused: true }], audio: ["firefox"], idle: 0 } };
          if (r.cmd === "settings") {
            const { cmd: _, ...patch } = r as Record<string, unknown>;
            Object.assign(status.settings, patch);
            setTimeout(() => emit("daemon-event", { event: "status", status: structuredClone(status) }), 50);
          }
          return { ok: true };
        }
        case "properties":
          return { output: a.output ?? null, properties: props };
        case "conditions": {
          const values = a.values as Values;
          return Object.fromEntries(Object.keys(a.conditions as object).map((k) => [k, !!values.rain]));
        }
        case "resolutions":
          return Object.fromEntries(items.map((i, n) => [i.id, n % 4 === 0 ? [3840, 2160] : [1920, 1080]]));
        case "folders":
          return [{ name: "Calm", items: ["1002", "1004", "1013"], folders: [] }, { name: "Animated", items: ["1016", "1017"], folders: [] }];
        case "presets":
          return [];
        case "ui_state":
          return ui;
        case "save_ui_state":
          ui = a.state;
          return null;
        case "autostart":
          return true;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  setTimeout(() => emit("daemon-event", { event: "status", status }), 300);
}
