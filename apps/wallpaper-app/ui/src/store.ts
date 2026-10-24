// The app's state: the library, the daemon's status and the selection.
import { create } from "zustand";
import { api, onDaemonDown, onDaemonEvent, type Item, type Status } from "./api";

interface AppState {
  items: Item[];
  loading: boolean;
  status: Status | null;
  /** Why the daemon isn't reachable, while it isn't. */
  daemonDown: string | null;
  selected: string | null;
  select: (id: string | null) => void;
  apply: (item: Item, output?: string) => Promise<void>;
}

export const useApp = create<AppState>()((set) => ({
  items: [],
  loading: true,
  status: null,
  daemonDown: null,
  selected: null,
  select: (id) => set({ selected: id }),
  apply: async (item, output) => {
    await api.daemon({ cmd: "set", wallpaper: item.path, output });
  },
}));

/** Load the library and follow the daemon. Call once at startup. */
export async function start() {
  await onDaemonEvent((e) => {
    if (e.event === "status") useApp.setState({ status: e.status, daemonDown: null });
  });
  await onDaemonDown((message) => useApp.setState({ status: null, daemonDown: message }));
  api
    .daemon({ cmd: "status" })
    .then((r) => r.status && useApp.setState({ status: r.status, daemonDown: null }))
    .catch((e) => useApp.setState({ daemonDown: String(e) }));
  const items = await api.library();
  useApp.setState({ items, loading: false });
}
