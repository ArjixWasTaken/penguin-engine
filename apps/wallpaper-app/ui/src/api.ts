// The backend's commands and events (see ../API.md), typed.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Kind = "scene" | "video" | "web" | "application" | "preset" | "unknown";
export type Rating = "everyone" | "questionable" | "mature" | "unknown";
export type Source = "workshop" | "projects" | "user";
export type Fill = "auto" | "crop" | "fit" | "stretch";
export type Layer = "background" | "bottom";

export interface Item {
  id: string;
  path: string;
  title: string;
  description: string | null;
  kind: Kind;
  tags: string[];
  rating: Rating;
  preview: string | null;
  size: number;
  updated: number | null;
  installed: number | null;
  workshop_id: string | null;
  source: Source;
  audio_reactive: boolean;
  has_properties: boolean;
  dependency: string | null;
}

export interface Settings {
  fps: number;
  audio: boolean;
  volume: number;
  fill: Fill;
  layer: Layer;
  passthrough: boolean;
}

export interface WallpaperInfo {
  path: string;
  workshop_id: string | null;
  title: string;
  type: string;
  dependency?: string;
  sound?: boolean;
  volume?: number;
}

export interface OutputStatus {
  name: string;
  width: number;
  height: number;
  scale: number;
  wallpaper: WallpaperInfo | null;
  paused: boolean;
  fps: number;
  fill: Fill;
  loading: string | null;
}

export interface Status {
  settings: Settings;
  outputs: OutputStatus[];
}

export type Value = boolean | number | string | null;
export type Values = Record<string, Value>;

export interface Property {
  name: string;
  type: string;
  label: string;
  text: string | null;
  value: Value;
  default: Value;
  options?: { label: string | null; value: Value }[];
  min?: number;
  max?: number;
  step?: number;
  order?: number;
  condition?: string;
}

export interface Folder {
  name: string;
  /** Item ids. */
  items: string[];
  folders: Folder[];
}

export interface Preset {
  name: string;
  wallpaper: string;
  values: Values;
}

/** A request in the daemon's control protocol. */
export type Request =
  | { cmd: "status" }
  | { cmd: "version" }
  | { cmd: "properties"; output?: string }
  | { cmd: "set"; output?: string; wallpaper: string; properties?: Values }
  | { cmd: "property"; output?: string; name: string; value: Value }
  | { cmd: "set-properties"; output?: string; values: Values }
  | { cmd: "reset-properties"; output?: string }
  | { cmd: "pause" | "resume"; output?: string }
  | { cmd: "settings"; fps?: number; audio?: boolean; volume?: number; fill?: Fill; layer?: Layer; passthrough?: boolean }
  | { cmd: "wallpaper-volume"; output?: string; volume: number }
  | { cmd: "fill"; output?: string; fill: Fill };

export interface Response {
  ok: boolean;
  error?: string;
  status?: Status;
  properties?: { output: string; wallpaper: string; properties: Property[] }[];
  version?: { protocol: number; daemon: string; features: string[] };
}

export type DaemonEvent =
  | { event: "status"; status: Status }
  | { event: "loading"; output: string; wallpaper: string }
  | { event: "load-failed"; output: string; wallpaper: string; error: string }
  | { event: "properties"; output: string; wallpaper: string; values: Values };

export const api = {
  library: (refresh = false) => invoke<Item[]>("library", { refresh }),
  daemon: (request: Request) => invoke<Response>("daemon", { request }),
  properties: (id: string, output?: string) =>
    invoke<{ output: string | null; properties: Property[] }>("properties", { id, output }),
  conditions: (conditions: Record<string, string>, values: Values) =>
    invoke<Record<string, boolean>>("conditions", { conditions, values }),
  resolution: (id: string) => invoke<[number, number] | null>("resolution", { id }),
  resolutions: () => invoke<Record<string, [number, number]>>("resolutions"),
  folders: () => invoke<Folder[]>("folders"),
  saveFolders: (folders: Folder[]) => invoke<void>("save_folders", { folders }),
  autostart: () => invoke<boolean>("autostart"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  presets: (id: string) => invoke<Preset[]>("presets", { id }),
  savePreset: (preset: Preset) => invoke<void>("save_preset", { preset }),
  deletePreset: (id: string, name: string) => invoke<void>("delete_preset", { id, name }),
  uiState: () => invoke<unknown>("ui_state"),
  saveUiState: (state: unknown) => invoke<void>("save_ui_state", { state }),
  openWorkshop: (id: string) => invoke<void>("open_workshop", { id }),
  openFolder: (id: string) => invoke<void>("open_folder", { id }),
};

/** Set by the browser preview (`mock.ts`): pictures are made up. */
const mocked = () => !!(window as unknown as { __PENGUIN_MOCK__?: boolean }).__PENGUIN_MOCK__;
let mockPicture: ((url: string) => string) | null = null;
export function setMockPictures(f: (url: string) => string) {
  mockPicture = f;
}
const picture = (scheme: string, id: string) => {
  const url = `${scheme}://localhost/${encodeURIComponent(id)}`;
  return mocked() && mockPicture ? mockPicture(url) : url;
};

/** A small still of a wallpaper (cached PNG). */
export const thumbUrl = (id: string) => picture("thumb", id);
/** A wallpaper's preview as it is (GIFs animate). */
export const previewUrl = (id: string) => picture("preview", id);

export function onDaemonEvent(f: (e: DaemonEvent) => void): Promise<UnlistenFn> {
  return listen<DaemonEvent>("daemon-event", ({ payload }) => f(payload));
}

export function onDaemonDown(f: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>("daemon-down", ({ payload }) => f(payload));
}
