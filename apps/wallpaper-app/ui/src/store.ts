// The app's state: the library, the daemon's status, what the browser
// shows and what the user chose. Part of it is kept between runs.
import { create } from "zustand";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  api,
  onDaemonDown,
  onDaemonEvent,
  type Folder,
  type Item,
  type Status,
  type Values,
} from "./api";
import type { Filters, Resolutions, Sort } from "./filter";
import { useProps } from "./props";

export type Tab = "library" | "discover" | "workshop";
export type SettingsTab = "General" | "Performance" | "Displays" | "Audio" | "About";

export interface Playlist {
  name: string;
  items: string[];
  mode: "Timer" | "Time of day" | "Each login";
  /** Minutes between changes (timer mode). */
  interval: number;
  order: "Sequential" | "Random";
  transition: boolean;
}

export const emptyPlaylist = (): Playlist => ({
  name: "",
  items: [],
  mode: "Timer",
  interval: 15,
  order: "Sequential",
  transition: true,
});

/** What is kept between runs (`save_ui_state`). */
interface Saved extends Filters {
  sort: Sort;
  asc: boolean;
  filtersOpen: boolean;
  open: Record<string, boolean>;
  selected: string | null;
  panelOpen: boolean;
  /** The output "Apply" changes; null for all of them. */
  target: string | null;
  playlist: Playlist;
  playlists: Playlist[];
  tileMin: number;
}

/** How each output looked when the app opened, for Cancel. */
interface Snapshot {
  [output: string]: { path: string; values: Values };
}

interface State extends Saved {
  items: Item[];
  byId: Map<string, Item>;
  resolutions: Resolutions;
  folders: Folder[];
  loading: boolean;
  status: Status | null;
  daemonDown: string | null;
  daemonVersion: string | null;
  daemonFeatures: string[];
  snapshot: Snapshot | null;
  tab: Tab;
  /** Open folder, by names from the top. */
  folderPath: string[];
  modal: "settings" | "playlist" | null;
  settingsTab: SettingsTab;
  toast: { text: string; error: boolean } | null;
}

const saved: Saved = {
  search: "",
  only: {},
  off: { "type:Application": true },
  favorites: {},
  sort: "Subscription date",
  asc: false,
  filtersOpen: true,
  open: { source: true, tags: false },
  selected: null,
  panelOpen: true,
  target: null,
  playlist: emptyPlaylist(),
  playlists: [],
  tileMin: 180,
};

export const useApp = create<State>()(() => ({
  ...saved,
  items: [],
  byId: new Map(),
  resolutions: {},
  folders: [],
  loading: true,
  status: null,
  daemonDown: null,
  daemonVersion: null,
  daemonFeatures: [],
  snapshot: null,
  tab: "library",
  folderPath: [],
  modal: null,
  settingsTab: "General",
  toast: null,
}));

const set = useApp.setState;
const get = useApp.getState;

let toastTimer: number | undefined;
export function toast(text: string, error = false) {
  clearTimeout(toastTimer);
  set({ toast: { text, error } });
  toastTimer = window.setTimeout(() => set({ toast: null }), error ? 4500 : 2200);
}

/** Run a daemon request, telling the user when it fails. */
export async function request(r: Parameters<typeof api.daemon>[0]) {
  try {
    return await api.daemon(r);
  } catch (e) {
    toast(String(e), true);
    return null;
  }
}

export function select(id: string | null) {
  if (get().selected === id) return;
  set({ selected: id });
  refreshProps();
}

export function toggleFavorite(id: string) {
  set((s) => {
    const favorites = { ...s.favorites };
    if (favorites[id]) delete favorites[id];
    else favorites[id] = true;
    return { favorites };
  });
}

/** The output showing `item` among those "Apply" targets, if any. */
export function liveOutput(item: Item | undefined): string | null {
  const s = get();
  if (!item || !s.status) return null;
  const out = s.status.outputs.find(
    (o) => (s.target === null || o.name === s.target) && o.wallpaper?.path === item.path,
  );
  return out?.name ?? null;
}

export function targetLabel(): string {
  return get().target ?? "all displays";
}

/** Show `item` on the target outputs, with the properties edited for it. */
export async function apply(item: Item) {
  const props = useProps.getState();
  const properties = props.itemId === item.id && !props.output ? props.changed() : undefined;
  const r = await request({
    cmd: "set",
    wallpaper: item.path,
    output: get().target ?? undefined,
    properties,
  });
  if (r) toast(`Applied “${item.title}” to ${targetLabel()}`);
}

/** Reload the selected wallpaper's properties (live or from its project). */
export function refreshProps() {
  const s = get();
  const item = s.selected ? s.byId.get(s.selected) : undefined;
  useProps.getState().load(item ?? null, liveOutput(item));
}

/** Put the outputs back as they were when the app opened. */
export async function revert() {
  const { snapshot, status } = get();
  if (!snapshot || !status) return;
  for (const out of status.outputs) {
    const was = snapshot[out.name];
    if (!was) continue;
    if (out.wallpaper?.path !== was.path) {
      await request({ cmd: "set", output: out.name, wallpaper: was.path, properties: was.values });
    } else if (Object.keys(was.values).length) {
      await request({ cmd: "set-properties", output: out.name, values: was.values });
    }
  }
}

async function takeSnapshot(status: Status) {
  const snapshot: Snapshot = {};
  const r = await api.daemon({ cmd: "properties" }).catch(() => null);
  for (const out of status.outputs) {
    if (!out.wallpaper) continue;
    const values: Values = {};
    for (const p of r?.properties?.find((x) => x.output === out.name)?.properties ?? []) values[p.name] = p.value;
    snapshot[out.name] = { path: out.wallpaper.path, values };
  }
  set({ snapshot });
}

export async function closeWindow() {
  await getCurrentWindow().close();
}

function onStatus(status: Status) {
  const first = get().status === null && get().snapshot === null;
  const before = get().status;
  // The target output went away.
  const target = get().target;
  set({
    status,
    daemonDown: null,
    target: target && !status.outputs.some((o) => o.name === target) ? null : target,
  });
  if (first) {
    takeSnapshot(status);
    api.daemon({ cmd: "version" }).then(
      (r) => set({ daemonVersion: r.version?.daemon ?? null, daemonFeatures: r.version?.features ?? [] }),
      () => {},
    );
  }
  // Whether the selected wallpaper is showing may have changed.
  const item = get().selected ? get().byId.get(get().selected!) : undefined;
  const wasLive = useProps.getState().output;
  const nowLive = liveOutput(item);
  if (wasLive !== nowLive || !before) refreshProps();
}

const SAVED_KEYS = Object.keys(saved) as (keyof Saved)[];
let saveTimer: number | undefined;

/** Load everything and follow the daemon. Call once at startup. */
export async function start() {
  const restored = (await api.uiState().catch(() => null)) as Partial<Saved> | null;
  if (restored && typeof restored === "object") {
    const pick: Partial<Saved> = {};
    for (const k of SAVED_KEYS) if (k in restored) (pick as Record<string, unknown>)[k] = restored[k];
    set(pick);
  }
  useApp.subscribe((s, prev) => {
    if (!SAVED_KEYS.some((k) => s[k] !== prev[k])) return;
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => {
      const st = get();
      const out: Partial<Saved> = {};
      for (const k of SAVED_KEYS) (out as Record<string, unknown>)[k] = st[k];
      api.saveUiState(out).catch(() => {});
    }, 400);
  });

  await onDaemonEvent((e) => {
    switch (e.event) {
      case "status":
        onStatus(e.status);
        break;
      case "load-failed":
        toast(`${e.output}: couldn't load the wallpaper: ${e.error}`, true);
        break;
      case "properties":
        useProps.getState().sync(e.output, e.values);
        break;
    }
  });
  await onDaemonDown((message) => set({ status: null, daemonDown: message }));
  api
    .daemon({ cmd: "status" })
    .then((r) => r.status && onStatus(r.status))
    .catch((e) => set({ daemonDown: String(e) }));

  const [items, folders] = await Promise.all([api.library(), api.folders().catch(() => [])]);
  set({ items, byId: new Map(items.map((i) => [i.id, i])), folders, loading: false });
  if (get().selected && !get().byId.has(get().selected!)) set({ selected: null });
  refreshProps();
  api.resolutions().then((resolutions) => set({ resolutions }), () => {});
}
