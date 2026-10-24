// The library: search, sort, filters and the grid.
import {
  ArrowDownWideNarrow,
  ArrowUpNarrowWide,
  AudioLines,
  ChevronDown,
  ChevronRight,
  Heart,
  RotateCcw,
  Search,
  SearchX,
  SlidersHorizontal,
  X,
} from "lucide-preact";
import { useMemo } from "preact/hooks";
import { admits, commonResolutions, folderAt, groups, idsIn, SORTS, sorted, type Sort } from "../filter";
import { useApp } from "../store";
import { Grid, type Entry } from "./Grid";

const DEFAULT_OFF = { "type:Application": true };

function resetFilters() {
  useApp.setState({ only: {}, off: { ...DEFAULT_OFF }, search: "" });
}

export function Browse() {
  const s = useApp();
  const common = useMemo(() => commonResolutions(s.items, s.resolutions), [s.items, s.resolutions]);
  const filterGroups = useMemo(() => groups(s.items, s.resolutions, common), [s.items, s.resolutions, common]);

  const searching = s.search.trim().length > 0;
  const folder = folderAt(s.folders, s.folderPath);
  const entries: Entry[] = useMemo(() => {
    let pool = s.items;
    let subfolders = s.folders;
    if (!searching) {
      if (folder) {
        const ids = new Set(folder.items);
        pool = s.items.filter((i) => ids.has(i.id));
        subfolders = folder.folders;
      } else {
        const inFolders = idsIn(s.folders);
        pool = s.items.filter((i) => !inFolders.has(i.id));
      }
    }
    const words = s.search.toLowerCase().split(/\s+/).filter(Boolean);
    const filters = { search: s.search, only: s.only, off: s.off, favorites: s.favorites };
    const items = sorted(pool.filter((i) => admits(i, filters, s.resolutions, common, words)), s.sort, s.asc);
    const folders: Entry[] = searching ? [] : subfolders.map((f) => ({ folder: f }));
    return [...folders, ...items.map((item) => ({ item }))];
  }, [s.items, s.folders, folder, searching, s.search, s.only, s.off, s.favorites, s.resolutions, common, s.sort, s.asc]);

  const shownItems = entries.filter((e) => "item" in e).length;
  const activeCount =
    Object.values(s.only).filter(Boolean).length +
    Object.entries(s.off).filter(([k, v]) => v && !(k in DEFAULT_OFF)).length +
    Object.keys(DEFAULT_OFF).filter((k) => !s.off[k]).length;

  return (
    <div class="browse">
      <div class="toolbar">
        <div class="field search">
          <Search size={14} class="muted icon" />
          <input
            value={s.search}
            onInput={(e) => useApp.setState({ search: (e.target as HTMLInputElement).value })}
            placeholder="Search wallpapers or tags"
          />
          {searching && (
            <button class="icon-btn" onClick={() => useApp.setState({ search: "" })}>
              <X size={14} />
            </button>
          )}
        </div>
        <button class={"btn filter-btn" + (s.filtersOpen ? " on" : "")} onClick={() => useApp.setState({ filtersOpen: !s.filtersOpen })}>
          <SlidersHorizontal size={14} />
          Filters
          {activeCount > 0 && <span class="count-pill">{activeCount}</span>}
        </button>
        {folder && !searching && (
          <div class="crumbs">
            <button class="btn link" onClick={() => useApp.setState({ folderPath: [] })}>
              Library
            </button>
            {s.folderPath.map((name, i) => (
              <>
                <ChevronRight size={14} />
                {i === s.folderPath.length - 1 ? (
                  <span>{name}</span>
                ) : (
                  <button class="btn link" onClick={() => useApp.setState({ folderPath: s.folderPath.slice(0, i + 1) })}>
                    {name}
                  </button>
                )}
              </>
            ))}
          </div>
        )}
        <div class="grow" />
        <span class="result-count">{s.loading ? "loading…" : `${shownItems} wallpapers`}</span>
        <button class="btn square" style={{ height: 34, borderRadius: 6 }} title="Sort direction" onClick={() => useApp.setState({ asc: !s.asc })}>
          {s.asc ? <ArrowUpNarrowWide size={15} /> : <ArrowDownWideNarrow size={15} />}
        </button>
        <select class="field sort" value={s.sort} onChange={(e) => useApp.setState({ sort: (e.target as HTMLSelectElement).value as Sort })}>
          {SORTS.map((o) => (
            <option value={o}>{o}</option>
          ))}
        </select>
      </div>

      <div class="browse-body">
        {s.filtersOpen && (
          <div class="filters">
            <button class="btn soft tall" style={{ flex: "none" }} onClick={resetFilters}>
              <RotateCcw size={14} />
              Reset filters
            </button>
            <div class="filter-box">
              <div class="caps" style={{ marginBottom: 2 }}>Show only</div>
              {(
                [
                  ["fav", "My favorites", <Heart size={13} color="var(--fav)" />],
                  ["audio", "Audio responsive", <AudioLines size={13} color="var(--accent)" />],
                  ["custom", "Customizable", <SlidersHorizontal size={13} color="#b59cff" />],
                ] as const
              ).map(([k, label, icon]) => (
                <label class="check">
                  <input
                    type="checkbox"
                    checked={!!s.only[k]}
                    onChange={() => useApp.setState({ only: { ...s.only, [k]: !s.only[k] } })}
                  />
                  {icon}
                  {label}
                </label>
              ))}
            </div>
            {filterGroups.map((g) => {
              const open = !!s.open[g.key];
              const on = g.options.filter((o) => !s.off[g.key + ":" + o]).length;
              const setAll = (value: boolean) => {
                const off = { ...s.off };
                for (const o of g.options) off[g.key + ":" + o] = value;
                useApp.setState({ off });
              };
              return (
                <div class="stack" style={{ flex: "none" }}>
                  <button class="group-head" onClick={() => useApp.setState({ open: { ...s.open, [g.key]: !open } })}>
                    {open ? <ChevronDown size={14} class="muted" /> : <ChevronRight size={14} class="muted" />}
                    {g.label}
                    <span class="summary">{on === g.options.length ? "all" : `${on}/${g.options.length}`}</span>
                  </button>
                  {open && (
                    <div class="group-items">
                      <div style={{ display: "flex", gap: 12 }}>
                        <button class="btn link" onClick={() => setAll(false)}>All</button>
                        <button class="btn link" onClick={() => setAll(true)}>None</button>
                      </div>
                      {g.options.length === 0 && <span class="muted">…</span>}
                      {g.options.map((o) => {
                        const key = g.key + ":" + o;
                        return (
                          <label class="check">
                            <input type="checkbox" checked={!s.off[key]} onChange={() => useApp.setState({ off: { ...s.off, [key]: !s.off[key] } })} />
                            {o}
                          </label>
                        );
                      })}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}

        <div class="grid-wrap">
          {!s.loading && entries.length === 0 ? (
            <div class="empty">
              <SearchX size={28} />
              <div class="what">{s.items.length ? "Nothing matches these filters" : "No wallpapers found"}</div>
              {s.items.length > 0 && (
                <button class="btn" style={{ color: "var(--accent)" }} onClick={resetFilters}>
                  Reset filters
                </button>
              )}
            </div>
          ) : (
            <Grid entries={entries} tileMin={s.tileMin} />
          )}
        </div>
      </div>
    </div>
  );
}
