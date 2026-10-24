// The wallpaper grid. Only the rows in view (and a couple around them) are
// in the page: with every tile present, WebKit keeps every picture decoded.
import { Check, Heart } from "lucide-preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { previewUrl, thumbUrl, type Folder, type Item } from "../api";
import { countIn } from "../filter";
import { apply, select, useApp } from "../store";

const GAP = 8;
const SPARE_ROWS = 2;

export type Entry = { folder: Folder } | { item: Item };

export function Grid({ entries, tileMin }: { entries: Entry[]; tileMin: number }) {
  const scroller = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [top, setTop] = useState(0);

  useLayoutEffect(() => {
    const el = scroller.current!;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth - 4, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  // A new list starts at the top.
  useEffect(() => {
    scroller.current?.scrollTo({ top: 0 });
    setTop(0);
  }, [entries]);

  const cols = Math.max(1, Math.floor((size.w + GAP) / (tileMin + GAP)));
  const tile = size.w > 0 ? (size.w - GAP * (cols - 1)) / cols : tileMin;
  const rowH = tile + GAP;
  const rows = Math.ceil(entries.length / cols);
  const first = Math.max(0, Math.floor(top / rowH) - SPARE_ROWS);
  const last = Math.min(rows, Math.ceil((top + size.h) / rowH) + SPARE_ROWS);
  const shown = entries.slice(first * cols, last * cols);

  return (
    <div class="grid-scroll" ref={scroller} onScroll={(e) => setTop((e.currentTarget as HTMLDivElement).scrollTop)}>
      <div class="grid-space" style={{ height: Math.max(0, rows * rowH - GAP) }}>
        {shown.map((entry, n) => {
          const i = first * cols + n;
          const style = { width: tile, height: tile, top: Math.floor(i / cols) * rowH, left: (i % cols) * (tile + GAP) };
          return "folder" in entry ? (
            <FolderTile key={"f:" + entry.folder.name} folder={entry.folder} style={style} />
          ) : (
            <Tile key={entry.item.id} item={entry.item} style={style} />
          );
        })}
      </div>
    </div>
  );
}

function FolderTile({ folder, style }: { folder: Folder; style: Record<string, number> }) {
  return (
    <div class="folder-tile" style={style} onClick={() => useApp.setState((s) => ({ folderPath: [...s.folderPath, folder.name] }))}>
      <div class="folder-art">
        <div class="tab-part" />
        <div class="body-part" />
      </div>
      <div class="label">{folder.name}</div>
      <div class="n">{countIn(folder)} items</div>
    </div>
  );
}

function Tile({ item, style }: { item: Item; style: Record<string, number> }) {
  const selected = useApp((s) => s.selected === item.id);
  const fav = useApp((s) => !!s.favorites[item.id]);
  const showing = useApp((s) => !!s.status?.outputs.some((o) => o.wallpaper?.path === item.path));
  const res = useApp((s) => s.resolutions[item.id]);
  const [animate, setAnimate] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const animated = !!item.preview && /\.(gif|webp)$/i.test(item.preview);
  const kind = item.kind === "video" ? "VIDEO" : item.kind === "web" ? "WEB" : item.kind === "preset" ? "PRESET" : item.kind === "application" ? "APP" : null;

  return (
    <div
      class={"tile" + (selected ? " selected" : "")}
      style={style}
      title={item.title}
      onClick={() => select(item.id)}
      onDblClick={() => {
        select(item.id);
        apply(item);
      }}
      onMouseEnter={() => {
        if (animated) timer.current = window.setTimeout(() => setAnimate(true), 350);
      }}
      onMouseLeave={() => {
        clearTimeout(timer.current);
        setAnimate(false);
      }}
    >
      {item.preview && <img src={animate ? previewUrl(item.id) : thumbUrl(item.id)} alt="" draggable={false} />}
      <div class="badges">
        {showing && (
          <span class="badge on" title="Showing now">
            <Check size={12} />
          </span>
        )}
        {fav && (
          <span class="badge fav" title="Favorite">
            <Heart size={12} />
          </span>
        )}
      </div>
      <div class="corner">
        {kind && <span class="badge text">{kind}</span>}
        {res && res[0] >= 3840 && <span class="badge text">4K</span>}
      </div>
      <div class="title">{item.title}</div>
    </div>
  );
}
