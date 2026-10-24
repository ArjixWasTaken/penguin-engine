// The playlist bar and its settings. Playlists are kept by the app; the
// daemon doesn't play them yet.
import { ArrowDown, ArrowUp, FolderOpen, Plus, Save, Settings2, Trash2, X } from "lucide-preact";
import { useState } from "preact/hooks";
import { thumbUrl } from "../api";
import { KIND_LABEL, SOURCE_LABEL } from "../filter";
import { emptyPlaylist, select, toast, useApp, type Playlist } from "../store";
import { MenuButton, SectionHead, Segments, Toggle } from "./ui";

function summary(p: Playlist): string {
  const n = p.items.length;
  if (!n) return "empty";
  const how = p.mode === "Timer" ? `every ${p.interval} min` : p.mode.toLowerCase();
  return `${p.name ? p.name + " · " : ""}${n} wallpaper${n > 1 ? "s" : ""} · ${how}`;
}

export function PlaylistBar() {
  const playlist = useApp((s) => s.playlist);
  const playlists = useApp((s) => s.playlists);
  const selected = useApp((s) => s.selected);
  const byId = useApp((s) => s.byId);
  const [name, setName] = useState("");

  const add = () => {
    if (!selected) return toast("Select a wallpaper first");
    if (playlist.items.includes(selected)) return toast("Already in the playlist");
    useApp.setState({ playlist: { ...playlist, items: [...playlist.items, selected] } });
    toast(`Added “${byId.get(selected)?.title}” to the playlist`);
  };
  const save = (close: () => void) => {
    const n = name.trim();
    if (!n) return;
    const p = { ...playlist, name: n };
    useApp.setState({ playlist: p, playlists: [...playlists.filter((x) => x.name !== n), p].sort((a, b) => a.name.localeCompare(b.name)) });
    setName("");
    close();
    toast(`Saved playlist “${n}”`);
  };

  return (
    <div class="playlist-bar">
      <h2>Playlist</h2>
      <MenuButton
        class="btn"
        up
        label={
          <>
            <FolderOpen size={14} />
            Load
          </>
        }
        menu={(close) =>
          playlists.length === 0 ? (
            <div class="none">No saved playlists</div>
          ) : (
            playlists.map((p) => (
              <div class="entry">
                <button
                  onClick={() => {
                    useApp.setState({ playlist: { ...p } });
                    close();
                    toast(`Loaded playlist “${p.name}”`);
                  }}
                >
                  {p.name} <span class="muted mono" style={{ fontSize: 11 }}>{p.items.length}</span>
                </button>
                <button class="icon-btn" title="Delete" onClick={() => useApp.setState({ playlists: playlists.filter((x) => x !== p) })}>
                  <Trash2 size={13} />
                </button>
              </div>
            ))
          )
        }
      />
      <MenuButton
        class="btn"
        up
        disabled={playlist.items.length === 0}
        label={
          <>
            <Save size={14} />
            Save
          </>
        }
        menu={(close) => (
          <div class="row" style={{ padding: 4, width: 260 }}>
            <input
              class="field grow"
              style={{ height: 30 }}
              placeholder="Playlist name"
              value={name || playlist.name}
              autoFocus
              onInput={(e) => setName((e.target as HTMLInputElement).value)}
              onKeyDown={(e) => e.key === "Enter" && save(close)}
            />
            <button class="btn primary tall" onClick={() => save(close)}>
              Save
            </button>
          </div>
        )}
      />
      <button class="btn" onClick={() => useApp.setState({ modal: "playlist" })}>
        <Settings2 size={14} />
        Configure
      </button>
      <button class="btn primary" onClick={add}>
        <Plus size={14} />
        Add wallpaper
      </button>
      <div class="pl-thumbs">
        {playlist.items.slice(0, 12).map((id) => (
          <img class="pl-thumb" src={thumbUrl(id)} title={byId.get(id)?.title} onClick={() => select(id)} alt="" />
        ))}
        <span class="pl-summary">{summary(playlist)}</span>
      </div>
    </div>
  );
}

export function PlaylistDialog() {
  const playlist = useApp((s) => s.playlist);
  const byId = useApp((s) => s.byId);
  const close = () => useApp.setState({ modal: null });
  const update = (p: Partial<Playlist>) => useApp.setState({ playlist: { ...playlist, ...p } });
  const move = (i: number, d: number) => {
    const items = [...playlist.items];
    const j = i + d;
    if (j < 0 || j >= items.length) return;
    [items[i], items[j]] = [items[j], items[i]];
    update({ items });
  };
  const n = playlist.items.length;
  return (
    <div class="backdrop" onClick={close}>
      <div class="modal playlist" onClick={(e) => e.stopPropagation()}>
        <div class="modal-head">
          <div class="title">Configure playlist</div>
          <button class="modal-close" onClick={close}>
            <X size={15} />
          </button>
        </div>
        <div class="modal-body" style={{ gap: 16 }}>
          <div class="pl-row">
            <div class="label">Mode</div>
            <Segments value={playlist.mode} options={["Timer", "Time of day", "Each login"] as const} onChange={(mode) => update({ mode })} />
          </div>
          <div class="pl-row">
            <div class="label">Order</div>
            <Segments value={playlist.order} options={["Sequential", "Random"] as const} onChange={(order) => update({ order })} />
          </div>
          {playlist.mode === "Timer" && (
            <div class="pl-row">
              <div class="label">Change every</div>
              <input type="range" min={1} max={120} value={playlist.interval} onInput={(e) => update({ interval: +(e.target as HTMLInputElement).value })} />
              <span class="mono muted" style={{ width: 60, textAlign: "right" }}>{playlist.interval} min</span>
            </div>
          )}
          <div class="pl-row">
            <div class="label">Fade transition</div>
            <Toggle on={playlist.transition} onChange={(transition) => update({ transition })} />
          </div>
          <SectionHead>{`${n} wallpaper${n === 1 ? "" : "s"}`}</SectionHead>
          {n === 0 && (
            <div class="pl-empty">
              Select a wallpaper and press <span style={{ color: "var(--accent)" }}>Add wallpaper</span> to build a playlist.
            </div>
          )}
          <div class="stack">
            {playlist.items.map((id, i) => {
              const item = byId.get(id);
              return (
                <div class="pl-item">
                  <span class="n">{i + 1}</span>
                  <img src={thumbUrl(id)} alt="" />
                  <div class="what">
                    <div>{item?.title ?? id}</div>
                    <div>{item ? `${KIND_LABEL[item.kind]} · ${SOURCE_LABEL[item.source]}` : "Not installed"}</div>
                  </div>
                  <button onClick={() => move(i, -1)}>
                    <ArrowUp size={14} />
                  </button>
                  <button onClick={() => move(i, 1)}>
                    <ArrowDown size={14} />
                  </button>
                  <button class="remove" onClick={() => update({ items: playlist.items.filter((_, k) => k !== i) })}>
                    <X size={14} />
                  </button>
                </div>
              );
            })}
          </div>
        </div>
        <div class="modal-foot">
          <button class="btn big" style={{ color: "var(--danger)", background: "transparent" }} onClick={() => useApp.setState({ playlist: emptyPlaylist() })}>
            Clear
          </button>
          <div class="grow" />
          <button class="btn big" onClick={close}>
            Close
          </button>
          <button
            class="btn primary big"
            onClick={() => toast(n ? "Playing playlists comes with the daemon's playlist support (next up)" : "Add wallpapers first")}
          >
            Start playlist
          </button>
        </div>
      </div>
    </div>
  );
}
