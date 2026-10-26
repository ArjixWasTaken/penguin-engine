# Penguin Engine

> [!WARNING]
> This project is vibe-slop: it was written almost entirely by an AI coding agent.

Animated desktop wallpapers for Linux: plays [Wallpaper Engine](https://www.wallpaperengine.io/)
wallpapers (scene, video and web) as the desktop background on Wayland. Written in Rust on
[wgpu](https://wgpu.rs). Penguin Engine is an independent project, not affiliated with
Wallpaper Engine.

- `penguind`: the daemon showing the wallpapers (one per output), controlled over a socket
- `penguinctl`: its command-line client
- `penguin-engine`: the app to browse your wallpaper library, apply wallpapers and change
  their properties and the settings

Work in progress. Much of what Penguin Engine knows about Wallpaper Engine's formats and
behaviour comes from
[CaptSilver/wallpaper-scene-renderer](https://github.com/CaptSilver/wallpaper-scene-renderer).

## Tech stack

- **Rendering:** [wgpu](https://wgpu.rs); Wallpaper Engine's shaders are compiled to SPIR-V with
  glslang and checked with naga.
- **Desktop:** smithay-client-toolkit (Wayland layer-shell) and wayland-protocols-plasma (KDE's
  window list and fake input).
- **Video:** libmpv (libmpv2), with frames handed to the GPU through EGL.
- **Web wallpapers:** [Servo](https://servo.org).
- **SceneScript:** QuickJS (rquickjs).
- **Audio:** rodio for playback; libpulse and realfft for the audio spectrum.
- **Text layers:** cosmic-text.
- **File formats:** binrw, lz4_flex and image.
- **App UI:** [Preact](https://preactjs.com) and zustand, in a WebKitGTK webview.
- **Scene viewer:** winit.

## Installation

On Arch Linux, from the AUR:

- [penguin-engine](https://aur.archlinux.org/packages/penguin-engine): the latest release
- [penguin-engine-git](https://aur.archlinux.org/packages/penguin-engine-git): the latest commit

```sh
paru -S penguin-engine   # or yay, or makepkg
systemctl --user enable --now penguin-engine.socket penguin-engine.service
```

Elsewhere, build from source (see [The app](#the-app) and [systemd](#systemd)).

## Requirements

Rendering needs the stock assets shipped with Wallpaper Engine (shaders, effects, materials).
Nothing from Wallpaper Engine is bundled here. With a Steam install they are found automatically
across all configured Steam libraries (`steamapps/libraryfolders.vdf`).
They can also be set explicitly with `assets = "path/to/assets"` in `config.toml`, via
`WPE_ASSETS=/path/to/assets`, or passed with `--assets`.
Additional workshop folders can be configured with `workshop = ["..."]` or `WPE_WORKSHOP`.

Video textures need libmpv. Frames are decoded with VA-API and handed to wgpu as dma-bufs
without touching the CPU; this needs the Vulkan backend and a Mesa EGL driver. Otherwise
mpv decodes in software and frames are uploaded (`WPE_VIDEO=cpu` forces this path).

Web wallpapers are rendered with [Servo](https://servo.org) (the `web` cargo feature, on by
default; the first build downloads a prebuilt SpiderMonkey and takes a few minutes). To build
without them: `cargo build --release -p penguind --no-default-features`, likewise for
`scene-viewer`. Pages paint on
the GPU through an offscreen EGL context and frames reach wgpu as dma-bufs, like video frames;
pages animate in step with the wallpaper's frames, so the frame rate limit, pausing and hidden
outputs apply to them. `WPE_WEB=cpu` forces Mesa's software renderer (WebGL content doesn't show
there). Page media needs GStreamer.

## Status

Plays Wallpaper Engine scene, video and web wallpapers; application
wallpapers are not supported. Video wallpapers play their `.mp4` full screen through libmpv (GPU decoding, the
fill modes, sound with `--audio`). Scenes render with their own shaders, compiled from WE's
GLSL/HLSL dialect to SPIR-V and run through wgpu:

- image layers with materials, blend modes, `colorBlendMode`, compose/fullscreen layers
- image effect chains (ping-pong buffers, FBOs, copy commands, layer dependencies)
- sprite sheets, video textures (libmpv), text layers, scene bloom
- HDR scenes: 16-bit float colour buffers clamped on output, `bloomhdr*` settings and the
  HDR mip-chain bloom (`postprocessing` `ultra`/`displayhdr`)
- user properties, keyframe animations, camera parallax and shake
- sound layers and audio-reactive spectrum (`--audio`)
- particles (sprite, trail, rope) without geometry shaders
- puppet warp (`.mdl` skinned meshes, bone animations, blend shapes)
- 3D: model objects (`.mdl` meshes with depth testing and culling), perspective scenes and
  perspective layers, camera paths, reflections, point/spot/tube/directional lights with
  shadow maps and volumetrics (shadowed too), spot light cookies (projected textures),
  distance and height fog
- SceneScript (QuickJS) on any scene value, with the engine services scripts use:
  - layers: transforms, colour, visibility (groups included), text and layout properties,
    `createLayer` / `destroyLayer` / `sortLayer` / `setParent` (the frame is rebuilt)
  - animations: keyframe, sprite-sheet and puppet animation-layer playback control,
    `createAnimationLayer` / `playSingleAnimation`, animation events
  - puppet bones (by name or index, pose queries, `setBoneTransform`, local angles,
    springy physics impulses), attachments (`setParent(layer, attachment)`), blend shapes
  - custom geometry (`createModelData`, `applyData` / `replaceData`, `createLayer({model})`)
  - particles (`instance` overrides, `emitParticles`, play/pause/stop), material and
    effect constants, effect functions, effect visibility, scene settings
    (`thisScene.bloom`, `camerashake`, ...), camera transforms (2D and 3D)
  - video (`getVideoTexture()`), sound layers (play/pause/stop/volume), media players
    over MPRIS (media events and the `$mediaThumbnail` album-art texture)
  - cursor events and hit boxes, `localStorage` (persisted per wallpaper), user property
    changes (`Wallpaper::set_user_property`, `scene-viewer --property name=value`),
    `openUserShortcut` (only shortcuts the user bound), `applyGeneralSettings`

`penguind` plays them as the desktop background on Wayland (wlr-layer-shell), controlled at
runtime over a Unix socket (`penguinctl`, the `penguin-engine` app) and run as a systemd user
service. Not yet: playlists and X11 desktops.

Debugging aids: `WPE_SCRIPT_AUDIT=<file>` records script requests the runtime doesn't
handle, `WPE_FAKE_AUDIO=1` feeds a synthetic spectrum, `scene-viewer --frames N` runs N
frames (with clicks) before a `--screenshot`, `RUST_LOG=wpe_runtime=debug` logs where a
wallpaper's load time went (texture decoding, shaders, uploads, pipelines, scripts).

## Layout

| Path | Purpose |
|---|---|
| `crates/wpe-vfs` | `.pkg` archives and the layered, case-insensitive asset filesystem |
| `crates/wpe-assets` | TEXB textures, scene/project/material/effect JSON models |
| `crates/wpe-shader` | WE shader preprocessor, HLSL repairs, GLSL to SPIR-V (glslang), reflection (naga) |
| `crates/wpe-scene` | Scene graph, cameras, user properties, animations |
| `crates/wpe-render` | wgpu renderer: render targets, material draws, effect chains |
| `crates/wpe-text` | Text layer layout and rasterisation |
| `crates/wpe-particles` | Particle simulation and CPU vertex generation |
| `crates/wpe-mdl` | `.mdl` puppet/model parsing, bone animation and blend shapes |
| `crates/wpe-script` | SceneScript runtime (QuickJS) |
| `crates/wpe-audio` | Sound playback and audio spectrum |
| `crates/wpe-web` | Web wallpapers: Servo host thread, file serving, Wallpaper Engine's JS API |
| `crates/wpe-runtime` | Embeddable API tying loading, animation and rendering together |
| `crates/wpe-ipc` | `penguind`'s control protocol (requests, responses, client) |
| `crates/wpe-library` | The wallpaper library: Steam workshop items, folders, presets, thumbnails |
| `apps/scene-viewer` | Windowed scene viewer |
| `apps/penguind` | The daemon: desktop background (Wayland layer-shell), control socket, rules |
| `apps/penguinctl` | Command-line client of `penguind` |
| `apps/penguin-engine` | The app (Tauri; its page in `ui/`, Preact) |
| `systemd/` | systemd user units for `penguind` |
| `tools/wpe-cli` | Debug tooling (pkg/tex/scene/shader corpus scans) |

## Scene viewer

```sh
cargo run --release -p scene-viewer -- ~/.local/share/Steam/steamapps/workshop/content/431960/<id>
cargo run --release -p scene-viewer -- <workshop id>
cargo run --release -p scene-viewer -- <wallpaper> --screenshot out.png --size 1920x1080 --time 2.5
cargo run --release -p scene-viewer -- <wallpaper> --audio --fps 30 --stats
```

The assets directory is found automatically for common Steam installs; pass `--assets` otherwise.
Set `WPE_DUMP_SHADERS=<dir>` to dump the final GLSL of every compiled shader.

### Web wallpapers

A web wallpaper's directory is served at `http://<workshop id>.wallpaper.localhost/` (its own
local storage, kept under `<data dir>/penguin-engine/web`). Pages get Wallpaper Engine's
JavaScript API: `wallpaperPropertyListener` (`applyUserProperties`, `applyGeneralProperties`
with the frame rate limit, `setPaused`) and `wallpaperRegisterAudioListener`. They get the
pointer, all three mouse buttons and the scroll wheel, and lay out at the output's scale. Audio
listeners get the spectrum while audio is on; page sound follows the audio setting and volume.
`file:` URLs reach only the wallpaper's own files and the files and directories picked for its
`file`/`directory` properties (handed to pages as Wallpaper Engine does, for `file:///` + path;
directories also through `userDirectoryFilesAddedOrChanged` and
`wallpaperRequestRandomFileForProperty`).
`crates/wpe-web/examples/event-test` is a page logging what it receives. They may load from the internet, but not
from this machine or the local network (localhost, private and link-local addresses, and names
resolving to them); `WPE_WEB_ALLOW_HOSTS=nas.lan,192.168.1.10` lets chosen hosts through.
Navigating away from the wallpaper and popups are blocked.

## Desktop wallpaper (Wayland)

```sh
cargo run --release -p penguind -- ~/.local/share/Steam/steamapps/workshop/content/431960/<id>
cargo run --release -p penguind -- <wallpaper> --output DP-1 --fps 30 --fill fit --audio
cargo run --release -p penguind -- <wallpaper> --layer bottom --passthrough --property name=value
```

Needs a compositor with `zwlr_layer_shell_v1` (KDE Plasma, sway, Hyprland, niri, ...). It puts
one full-screen layer surface on every output (or only `--output <name>`), each with its own
`Wallpaper`, follows outputs being plugged in and out, renders at the output's (fractional)
scale, and paces frames with frame callbacks, so a compositor that stops sending them for
a hidden surface also stops the rendering. Cursor movement and left clicks over the surface
reach the wallpaper (parallax, script cursor events) unless `--passthrough` is given. Sound
(`--audio`) plays from the first output only. SIGINT/SIGTERM exit cleanly.

`--layer` picks the layer-shell layer: `background` (default) or `bottom`.

KDE Plasma (6, Wayland): KWin maps the layer-shell `background` layer to its desktop layer,
the same layer as plasmashell's desktop window (wallpaper, icons, desktop widgets), and
stacks windows within a layer by mapping order. Started after plasmashell, `penguind` on
`background` therefore shows above Plasma's own wallpaper, and above desktop icons and
widgets. It takes the desktop's left clicks unless `--passthrough`; right clicks still reach the
desktop and open its menu. If plasmashell restarts afterwards it covers the wallpaper again;
restart `penguind` then. `bottom` maps to KWin's below layer: always above Plasma's desktop,
still under normal windows and panels.

### Control socket

The daemon listens on `$XDG_RUNTIME_DIR/penguin-engine.sock` (`--socket` to change it).
`penguinctl` talks to it:

```sh
penguinctl status                        # outputs, wallpapers, fps, settings
penguinctl set 3566329932                # a workshop id or a path, on every output
penguinctl set --output DP-1 ~/walls/foo # one output
penguinctl properties                    # user properties, with types, ranges, options
penguinctl property sakura false         # value as JSON, or else a string
penguinctl properties-set a=1 b=true     # several at once, one rebuild
penguinctl reset-properties              # back to the wallpaper's (or preset's) values
penguinctl set --prop speed=2 3566329932 # load with property values
penguinctl pause | resume                # [--output X]
penguinctl audio on|off ; penguinctl volume 40 ; penguinctl fps 30
penguinctl particles 50                  # percent of the particles wallpapers ask for
penguinctl signals                       # windows, sound, battery, idle: what rules see
penguinctl wallpaper-volume 50           # [--output X] this wallpaper, % of the global volume
penguinctl fill fit                      # [--output X] auto | crop | fit | stretch
penguinctl reload                        # re-read config.toml (as SIGHUP does)
penguinctl quit
penguinctl version                       # daemon version, protocol, features (web)
penguinctl watch                         # events as JSON lines, until the daemon goes
penguinctl --json status                 # the raw response
```

The protocol (`crates/wpe-ipc`) is line-delimited JSON: one request object per line, tagged
by `cmd`, and one response per line, in order. Any number of clients can be connected.

Wallpapers load on worker threads while the current one keeps playing (only starting the
scripts runs on the render thread). A `set` is answered once the new wallpaper has loaded and
shows (`{"ok":true}`) or failed to load (an error, and the old wallpaper stays); meanwhile the
daemon goes on serving other clients, and that client's next requests wait their turn. A `set`
covering several outputs changes all of them or, if one fails, none. A newer `set` for an output
still loading supersedes the older request, which is answered with `"superseded by a later set
request"`. Wallpapers restored at startup load the same way, so a `status` right after starting
can show an output without a wallpaper yet.

```text
> {"cmd":"status"}
< {"ok":true,"status":{"settings":{"fps":0,"audio":false,"volume":100,"fill":"crop","layer":"background","passthrough":true},"outputs":[{"name":"DP-3","width":1920,"height":1080,"scale":1.0,"wallpaper":{"path":".../431960/3566329932","workshop_id":"3566329932","title":"Frieren ...","type":"scene"},"paused":false,"fps":106.2,"fill":"crop"}]}}
> {"cmd":"set","output":"DP-3","wallpaper":"3717623935"}
< {"ok":true}
> {"cmd":"property","name":"clock","value":false}
< {"ok":true}
> {"cmd":"properties"}
< {"ok":true,"properties":[{"output":"DP-3","wallpaper":"...","properties":[{"name":"musicvisualizer","type":"slider","text":"Music Visualizer","value":1,"default":1,"min":0.0,"max":1.0,"order":104.0}, ...]}]}
> {"cmd":"volume","volume":150}
< {"ok":false,"error":"volume 150 is out of 0..=100"}
```

Requests: `status`, `set` {`output`?, `wallpaper`, `properties`?}, `properties` {`output`?},
`property` {`output`?, `name`, `value`}, `set-properties` {`output`?, `values`},
`reset-properties` {`output`?}, `pause` / `resume` {`output`?}, `audio` {`enabled`}, `volume`
{`volume`}, `wallpaper-volume` {`output`?, `volume`}, `fps` {`fps`}, `fill` {`output`?, `fill`}, `settings` {`fps`?, `audio`?, `volume`?,
`fill`?, `layer`?, `passthrough`?, `particles`?, `rules`?}, `signals`, `reload-settings`,
`version`, `subscribe`, `quit`. Without
`output` a request applies to every output.

Protocol 2 (`version` answers `{"protocol":2,"daemon":"0.1.0","features":["web"]}`) only adds
to version 1. `set-properties` changes several properties with one rebuild: one value (or a web
wallpaper's, which its page takes as they come) applies in place; several on a scene reload it
off the render thread with all of them, answered once it shows. `reset-properties` returns to
the wallpaper's values and forgets the saved changes. `settings` changes several settings as
the single-setting requests do (a `fill` there is the global one, clearing outputs' own). A
status's outputs have `loading`, the wallpaper loading for them, if any.

`subscribe` is answered `{"ok":true}`; from then on the connection carries events, starting
with the current status, until it closes (other requests on it are ignored):

```text
< {"event":"status","status":{...}}          after any change, once per loop iteration
< {"event":"loading","output":"DP-3","wallpaper":"/.../3717623935"}
< {"event":"load-failed","output":"DP-3","wallpaper":"/...","error":"..."}
< {"event":"properties","output":"DP-3","wallpaper":"/...","values":{"clock":false,...}}
```

`properties` carries every property's current value, after a change and after a wallpaper
loads. A subscriber never slows the daemon down: one that leaves more than 1 MB unread is
disconnected.

Preset items (workshop items that only name another wallpaper, `dependency`, with property
values, `preset`) play that wallpaper with the preset's values under the saved changes; their
status shows the preset item's path, id and title, and the wallpaper playing as `dependency`.

### State and settings

What is shown where is saved to `$XDG_STATE_HOME/penguin-engine/state.json`
(`~/.local/state/...`): each output's wallpaper, its changed user properties and fill mode,
and the wallpaper set last, which new outputs get. Started without a wallpaper argument,
`penguind` restores it (outputs are matched by name); a wallpaper on the command line is shown
on every output and saved.

Settings live in `$XDG_CONFIG_HOME/penguin-engine/config.toml` (all optional):

```toml
fps = 0              # frame rate cap, 0 = follow the display
audio = false
volume = 100         # percent
fill = "auto"        # auto | crop | fit | stretch
layer = "background" # background | bottom
passthrough = false
particles = 100      # percent of the particles wallpapers ask for

# Custom Wallpaper Engine assets and workshop paths (optional; Steam installs
# across all libraries in libraryfolders.vdf are discovered automatically)
# assets = "/path/to/wallpaper_engine/assets"
# workshop = ["/path/to/steamapps/workshop/content/431960"]

# Rules hold wallpapers back while something else goes on (without any, the
# default pauses behind a fullscreen window). `when` is one of
#   { if = "window", state = "open" | "focused" | "maximized" | "fullscreen", app = "firefox" }
#   { if = "audio", app = "spotify" }     another app plays sound
#   { if = "battery", below = 20 }        on battery (below: only under that charge)
#   { if = "idle", seconds = 600 }        no input
# (`app` is optional); then `playback` (run | mute | pause | stop, which unloads the
# wallpaper), `fps`, `particles` (percent) and, for window states, `all_outputs`.
[[rules]]
when = { if = "window", state = "fullscreen" }
playback = "pause"

[[rules]]
when = { if = "battery" }
fps = 30
particles = 50
```

Window rules need KWin (KDE Plasma), which tells about other applications' windows; a window
counts on the output its centre is on. `penguinctl signals` shows what the rules see, and
`penguinctl status` which rules hold where.

Command-line flags win over it. Settings changed over the socket are saved in the state and win
over the file until the next reload (`penguinctl reload`, SIGHUP, `systemctl --user reload`),
which makes the file (and the flags) authoritative again. Everything except the layer on very
old compositors is applied live.

### The app

```sh
pnpm -C apps/penguin-engine/ui install && pnpm -C apps/penguin-engine/ui build
cargo run --release -p penguin-engine
```

It lists the wallpapers Steam installed (and Wallpaper Engine's own projects), applies them
through `penguind`, edits their properties live and keeps the settings (including the rules).
`apps/penguin-engine/API.md` describes the backend the page talks to.

### systemd

`systemd/` has user units: `penguin-engine.socket` (the control socket, so `penguinctl`
starts the daemon on demand) and `penguin-engine.service` (`Type=notify`: ready once the
outputs render, a status line with what plays where, a watchdog, `ExecReload` sends SIGHUP).
Both are part of `graphical-session.target`, which KDE Plasma, GNOME and systemd-managed
sway/Hyprland sessions start.

```sh
cargo install --path apps/penguind && cargo install --path apps/penguinctl
penguind 3566329932    # once, to pick a wallpaper (Ctrl-C), or later: penguinctl set ...
systemctl --user link $PWD/systemd/penguin-engine.socket $PWD/systemd/penguin-engine.service
systemctl --user enable --now penguin-engine.socket penguin-engine.service
systemctl --user status penguin-engine   # Status: "DP-1: <title>"
journalctl --user -u penguin-engine
```

`ExecStart` points at `~/.cargo/bin/penguind`; edit it (`systemctl --user edit --full
penguin-engine.service`) to pass flags or use another path. The session's
`WAYLAND_DISPLAY` must be in the user manager's environment (Plasma and most compositors
import it). To uninstall: `systemctl --user disable --now penguin-engine.socket
penguin-engine.service`.

## Tools

```sh
cargo run -p wpe-cli -- ls path/to/scene.pkg
cargo run -p wpe-cli -- extract path/to/scene.pkg out/
cargo run -p wpe-cli -- scan-pkgs ~/.local/share/Steam/steamapps/workshop/content/431960
cargo run -p wpe-cli -- tex2png --pkg path/to/scene.pkg materials/foo.tex out.png
cargo run -p wpe-cli -- scan-tex --assets <WE assets> --workshop <workshop dir>
cargo run -p wpe-cli -- scan-scenes --assets <WE assets> --workshop <workshop dir>
cargo run -p wpe-cli -- scan-shaders --assets <WE assets> --workshop <workshop dir>
```

## License

GPL-2.0-only; see `LICENSE`.
