import { getCurrentWindow } from "@tauri-apps/api/window";
import { ChevronsLeft, ChevronsRight, Minus, Square, X } from "lucide-preact";
import { useApp } from "../store";

const VERSION = "0.1.0";

export function TitleBar() {
  const panelOpen = useApp((s) => s.panelOpen);
  const win = getCurrentWindow();
  return (
    <div class="titlebar" data-tauri-drag-region>
      <div class="logo" data-tauri-drag-region />
      <div class="name" data-tauri-drag-region>Penguin Engine</div>
      <div class="version" data-tauri-drag-region>v{VERSION}</div>
      <div class="grow" data-tauri-drag-region />
      <button class="winbtn panel" title="Toggle details" onClick={() => useApp.setState({ panelOpen: !panelOpen })}>
        {panelOpen ? <ChevronsRight size={15} /> : <ChevronsLeft size={15} />}
      </button>
      <div style={{ display: "flex" }}>
        <button class="winbtn" title="Minimize" onClick={() => win.minimize()}>
          <Minus size={15} />
        </button>
        <button class="winbtn" title="Maximize" onClick={() => win.toggleMaximize()}>
          <Square size={12} />
        </button>
        <button class="winbtn close" title="Close" onClick={() => win.close()}>
          <X size={15} />
        </button>
      </div>
    </div>
  );
}

export { VERSION };
