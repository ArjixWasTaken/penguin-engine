// Which wallpapers the browser shows, in what order.
import type { Folder, Item, Kind, Rating, Source } from "./api";

export const KIND_LABEL: Record<Kind, string> = {
  scene: "Scene",
  video: "Video",
  web: "Web",
  application: "Application",
  preset: "Preset",
  unknown: "Scene",
};
export const RATING_LABEL: Record<Rating, string> = {
  everyone: "Everyone",
  questionable: "Questionable",
  mature: "Mature",
  unknown: "Unrated",
};
export const SOURCE_LABEL: Record<Source, string> = {
  workshop: "Workshop",
  projects: "Official",
  user: "My Wallpapers",
};

export const SORTS = ["Subscription date", "Name", "File size", "Last updated"] as const;
export type Sort = (typeof SORTS)[number];

export type Resolutions = Record<string, [number, number]>;

export interface Filters {
  search: string;
  /** "Show only" switches. */
  only: { fav?: boolean; audio?: boolean; custom?: boolean };
  /** `group:option` keys switched off. */
  off: Record<string, boolean>;
  favorites: Record<string, boolean>;
}

export interface Group {
  key: string;
  label: string;
  options: string[];
}

const UNTAGGED = "Untagged";

export function resolutionLabel(r: [number, number] | undefined, common: Set<string>): string {
  if (!r) return "Unknown";
  const label = `${r[0]} x ${r[1]}`;
  return common.has(label) ? label : "Other";
}

/** The most common sizes get their own option; the rest are "Other". */
export function commonResolutions(items: Item[], res: Resolutions): Set<string> {
  const counts = new Map<string, number>();
  for (const i of items) {
    const r = res[i.id];
    if (r) counts.set(`${r[0]} x ${r[1]}`, (counts.get(`${r[0]} x ${r[1]}`) ?? 0) + 1);
  }
  return new Set([...counts].sort((a, b) => b[1] - a[1]).slice(0, 6).map(([k]) => k));
}

/** The filter groups, with the options the library has. */
export function groups(items: Item[], res: Resolutions, common: Set<string>): Group[] {
  const present = <T,>(f: (i: Item) => T[]) => [...new Set(items.flatMap(f))];
  const kinds = (["scene", "video", "web", "preset", "application"] as Kind[]).map((k) => KIND_LABEL[k]);
  const ratings = (["everyone", "questionable", "mature", "unknown"] as Rating[]).map((r) => RATING_LABEL[r]);
  const sizes = present((i) => [resolutionLabel(res[i.id], common)]).sort((a, b) => {
    const rank = (s: string) => (s === "Other" ? 1 : s === "Unknown" ? 2 : 0);
    return rank(a) - rank(b) || a.localeCompare(b, undefined, { numeric: true });
  });
  const tagCounts = new Map<string, number>();
  for (const i of items) for (const t of i.tags.length ? i.tags : [UNTAGGED]) tagCounts.set(t, (tagCounts.get(t) ?? 0) + 1);
  const tags = [...tagCounts.keys()].sort((a, b) => (a === UNTAGGED ? 1 : b === UNTAGGED ? -1 : a.localeCompare(b)));
  return [
    { key: "type", label: "Type", options: kinds.filter((k) => items.some((i) => KIND_LABEL[i.kind] === k)) },
    { key: "age", label: "Age rating", options: ratings.filter((r) => items.some((i) => RATING_LABEL[i.rating] === r)) },
    { key: "res", label: "Resolution", options: sizes },
    { key: "source", label: "Source", options: (["workshop", "projects", "user"] as Source[]).filter((s) => items.some((i) => i.source === s)).map((s) => SOURCE_LABEL[s]) },
    { key: "tags", label: "Tags", options: tags },
  ];
}

export function admits(item: Item, f: Filters, res: Resolutions, common: Set<string>, words: string[]): boolean {
  const off = f.off;
  if (f.only.fav && !f.favorites[item.id]) return false;
  if (f.only.audio && !item.audio_reactive) return false;
  if (f.only.custom && !item.has_properties) return false;
  if (off["type:" + KIND_LABEL[item.kind]]) return false;
  if (off["age:" + RATING_LABEL[item.rating]]) return false;
  if (off["res:" + resolutionLabel(res[item.id], common)]) return false;
  if (off["source:" + SOURCE_LABEL[item.source]]) return false;
  const tags = item.tags.length ? item.tags : [UNTAGGED];
  if (!tags.some((t) => !off["tags:" + t])) return false;
  return words.every(
    (w) =>
      item.title.toLowerCase().includes(w) ||
      item.tags.some((t) => t.toLowerCase().includes(w)) ||
      item.workshop_id === w,
  );
}

export function sorted(items: Item[], sort: Sort, asc: boolean): Item[] {
  const key: Record<Sort, (i: Item) => number | string> = {
    "Subscription date": (i) => -(i.installed ?? 0),
    Name: (i) => i.title.toLowerCase(),
    "File size": (i) => -i.size,
    "Last updated": (i) => -(i.updated ?? 0),
  };
  const f = key[sort];
  const list = [...items].sort((a, b) => {
    const x = f(a), y = f(b);
    return x < y ? -1 : x > y ? 1 : 0;
  });
  return asc ? list.reverse() : list;
}

/** The folder at `path` (names from the top), if it exists. */
export function folderAt(folders: Folder[], path: string[]): Folder | null {
  let level: Folder[] = folders;
  let found: Folder | null = null;
  for (const name of path) {
    found = level.find((f) => f.name === name) ?? null;
    if (!found) return null;
    level = found.folders;
  }
  return found;
}

/** Every item id in `folders` and their subfolders. */
export function idsIn(folders: Folder[], into = new Set<string>()): Set<string> {
  for (const f of folders) {
    f.items.forEach((id) => into.add(id));
    idsIn(f.folders, into);
  }
  return into;
}

export function countIn(f: Folder): number {
  return idsIn([f]).size;
}

export function humanSize(bytes: number): string {
  const mb = bytes / 1048576;
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  if (mb >= 1) return `${Math.round(mb)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}
