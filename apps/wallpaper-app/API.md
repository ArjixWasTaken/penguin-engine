# Penguin Engine: the page's API

The frontend in `ui/` is Preact + zustand, built with Vite (pnpm).
`ui/src/api.ts` wraps everything below with types; `ui/src/store.ts`
holds the app state.

- Develop: `pnpm -C apps/wallpaper-app/ui install`, then
  `pnpm -C apps/wallpaper-app/ui dev` and `cargo run -p wallpaper-app`
  (debug builds load the dev server at `localhost:1420` and reload on edit).
- Release: `pnpm -C apps/wallpaper-app/ui build`, then
  `cargo build --release -p wallpaper-app` (embeds `ui/dist`).
- Without the app: open `http://localhost:1420` in a browser while
  `pnpm dev` runs. Outside Tauri the page answers its own commands with
  made-up wallpapers and two displays (`ui/src/mock.ts`).

## Commands (`await invoke(name, args)`)

Errors reject with a message string.

| Command | Args | Returns |
|---|---|---|
| `library` | `{refresh?: bool}` | `Item[]` (below), sorted by title. Scanned once, cached; `refresh` rescans. |
| `daemon` | `{request}` | The daemon's `Response`. `request` is the control protocol's JSON (README, "Control protocol"): `{cmd: "status"}`, `{cmd: "set", output?, wallpaper: item.path, properties?}`, `{cmd: "property", output?, name, value}`, `{cmd: "set-properties", output?, values}`, `{cmd: "reset-properties", output?}`, `{cmd: "pause"/"resume", output?}`, `{cmd: "settings", fps?, audio?, volume?, fill?, layer?, passthrough?}`, `{cmd: "wallpaper-volume", output?, volume}`, `{cmd: "fill", output?, fill}`, `{cmd: "version"}`. Rejects when the daemon isn't running or says no. |
| `properties` | `{id, output?}` | `{output, properties: Property[]}`. With `output` (one showing this wallpaper): the live values; without: the project's (a preset item's values). |
| `conditions` | `{conditions: {name: expr}, values: {name: value}}` | `{name: bool}`: which properties show (`Property.condition`) with these values. |
| `resolution` | `{id}` | `[width, height]` or null. Reads the wallpaper's files: ask for the selected item only. |
| `resolutions` | | `{id: [width, height]}` for every wallpaper with a known size. The first call reads every wallpaper (a second or two). |
| `folders` | | `Folder[]`: `{name, items: id[], folders: Folder[]}`. The first time, Wallpaper Engine's own folders are imported. |
| `save_folders` | `{folders}` | |
| `autostart` | | Whether the daemon's systemd units are enabled. |
| `set_autostart` | `{enabled}` | Enables or disables them. |
| `presets` | `{id}` | `Preset[]`: `{name, wallpaper: id, values: {name: value}}`. |
| `save_preset` | `{preset}` | Saves (replacing one of the same name). |
| `delete_preset` | `{id, name}` | |
| `ui_state` | | What `save_ui_state` stored (null at first). |
| `save_ui_state` | `{state}` | Any JSON (search, filters, selection, sizes). |
| `open_workshop` | `{id}` | Opens the Steam page (workshop items). |
| `open_folder` | `{id}` | Opens the wallpaper's directory. |

`Item`: `{id, path, title, description, kind: "scene"|"video"|"web"|"application"|"preset"|"unknown", tags: string[], rating: "everyone"|"questionable"|"mature"|"unknown", preview, size (bytes), updated, installed (unix seconds or null), workshop_id, source: "workshop"|"projects"|"user", audio_reactive, has_properties, dependency}`.

`Property`: `{name, type: "bool"|"slider"|"combo"|"color"|"textinput"|"text"|"group"|"file"|"directory"|"scenetexture"|..., label (plain text, translated), text, value, default, options?: [{label, value}], min?, max?, step?, order?, condition?}`. Already ordered. Colours are `"r g b"` with components 0..1.

## Pictures

- `thumb://localhost/<id>`: a 400×225 PNG still (cached on disk).
- `preview://localhost/<id>`: the preview file itself (GIFs animate).

## Events (`listen(name, ({payload}) => ...)`)

- `daemon-event`: every event the daemon pushes: `{event: "status", status}` (first, and after any change), `{event: "loading", output, wallpaper}`, `{event: "load-failed", output, wallpaper, error}`, `{event: "properties", output, wallpaper, values}`. A status's outputs carry `wallpaper: {path, title, type, workshop_id, dependency?, sound, volume}`, `paused`, `fill`, `loading`.
- `daemon-down`: a message, while the daemon isn't running (repeated as it retries).

Ask `daemon({request: {cmd: "status"}})` at start too, in case the first event came before the page listened.

## Notes for the page

- **Render only the tiles in view** (plus a screen's worth around them).
  With all ~900 tiles in the page, WebKit keeps every decoded thumbnail:
  the placeholder page uses about 600 MB in WebKit's process.
- Throttle sliders: send a property or volume change, and while it is in
  flight only remember the newest value; send that when the reply comes.
- Load `preview://` (animated GIFs) for the hovered tile and the detail
  panel only.
