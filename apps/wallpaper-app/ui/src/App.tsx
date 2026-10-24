import { open as pick } from "@tauri-apps/plugin-dialog";
import { Check, Compass, HardDrive, Monitor, Package, Settings, Upload } from "lucide-preact";
import { useApp, closeWindow, revert, request, toast, apply, targetLabel, type Tab } from "./store";
import { Browse } from "./components/Browse";
import { Details } from "./components/Details";
import { SettingsDialog } from "./components/Settings";
import { TitleBar } from "./components/TitleBar";

const TABS: [Tab, string, typeof HardDrive][] = [
  ["library", "Library", HardDrive],
  ["discover", "Discover", Compass],
  ["workshop", "Workshop", Package],
];

async function openWallpaper() {
  const chosen = await pick({ directory: true, multiple: false, title: "Open a wallpaper folder" });
  if (typeof chosen !== "string") return;
  const r = await request({ cmd: "set", wallpaper: chosen, output: useApp.getState().target ?? undefined });
  if (r) toast(`Opened it on ${targetLabel()}`);
}

async function ok() {
  const s = useApp.getState();
  const item = s.selected ? s.byId.get(s.selected) : undefined;
  const showing = item && s.status?.outputs.some((o) => (s.target === null || o.name === s.target) && o.wallpaper?.path === item.path);
  if (item && !showing && s.status && item.kind !== "application") await apply(item);
  await closeWindow();
}

async function cancel() {
  await revert();
  await closeWindow();
}

export function App() {
  const tab = useApp((s) => s.tab);
  const panelOpen = useApp((s) => s.panelOpen);
  const modal = useApp((s) => s.modal);
  const toastState = useApp((s) => s.toast);
  const daemonDown = useApp((s) => s.daemonDown);

  return (
    <div class="app">
      <TitleBar />
      <div class="body">
        <div class="main">
          <div class="tabs">
            {TABS.map(([key, label, Icon]) => (
              <button class={"tab" + (tab === key ? " on" : "")} onClick={() => useApp.setState({ tab: key, folderPath: [] })}>
                <Icon size={15} />
                {label}
              </button>
            ))}
            <div class="grow" />
            <div class="tab-tools">
              <button class="btn" onClick={() => useApp.setState({ modal: "settings", settingsTab: "Displays" })}>
                <Monitor size={14} />
                Displays
              </button>
              <button class="btn" onClick={() => useApp.setState({ modal: "settings", settingsTab: "General" })}>
                <Settings size={14} />
                Settings
              </button>
            </div>
          </div>
          {daemonDown && (
            <div class="banner">
              <span class="grow">Not connected: {daemonDown}. Wallpapers can't be applied until it runs.</span>
            </div>
          )}
          {tab === "library" ? (
            <Browse />
          ) : (
            <div class="coming">
              {tab === "discover" ? <Compass size={30} /> : <Package size={30} />}
              <div class="what">{tab === "discover" ? "Discover" : "Workshop"} isn't here yet</div>
              <div>Browsing the Steam Workshop comes later. Wallpapers you subscribe to in Steam show up in the Library.</div>
            </div>
          )}
          <div class="actions">
            <button class="btn big wide" onClick={openWallpaper}>
              <Upload size={14} />
              Open Wallpaper
            </button>
            <div class="grow" />
            <button class="btn primary big ok" onClick={ok}>
              OK
            </button>
            <button class="btn big cancel" onClick={cancel}>
              Cancel
            </button>
          </div>
        </div>
        {panelOpen && <Details />}
      </div>
      {modal === "settings" && <SettingsDialog />}
      {toastState && (
        <div class={"toast" + (toastState.error ? " error" : "")}>
          {!toastState.error && <Check size={15} color="#1d8bb0" />}
          {toastState.text}
        </div>
      )}
    </div>
  );
}
