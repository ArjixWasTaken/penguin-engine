# wallpaper-engine-rs

A Rust ([wgpu](https://wgpu.rs) + [winit](https://github.com/rust-windowing/winit)) renderer for
[Wallpaper Engine](https://www.wallpaperengine.io/) *scene* wallpapers, aimed at Linux.

Work in progress. Format and behaviour knowledge is derived from
[CaptSilver/wallpaper-scene-renderer](https://github.com/CaptSilver/wallpaper-scene-renderer)
(C++/Vulkan, GPL-2.0), so this project is licensed GPL-2.0 as well.

## Requirements

Rendering needs the stock assets shipped with Wallpaper Engine (shaders, effects, materials).
Nothing from Wallpaper Engine is bundled here. With a Steam install they live at
`~/.local/share/Steam/steamapps/common/wallpaper_engine/assets`.

Video textures need libmpv. Frames are decoded with VA-API and handed to wgpu as dma-bufs
without touching the CPU; this needs the Vulkan backend and a Mesa EGL driver. Otherwise
mpv decodes in software and frames are uploaded (`WPE_VIDEO=cpu` forces this path).

## Status

Renders Wallpaper Engine scene wallpapers with their own shaders, compiled from WE's GLSL/HLSL
dialect to SPIR-V and run through wgpu:

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
  shadow maps and volumetrics
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

Not yet: light cookies, desktop background (wallpaper) mode and the wallpaper
picker UI.

Debugging aids: `WPE_SCRIPT_AUDIT=<file>` records script requests the runtime doesn't
handle, `WPE_FAKE_AUDIO=1` feeds a synthetic spectrum, `scene-viewer --frames N` runs N
frames (with clicks) before a `--screenshot`.

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
| `crates/wpe-runtime` | Embeddable API tying loading, animation and rendering together |
| `apps/scene-viewer` | Windowed scene viewer |
| `tools/wpe-cli` | Debug tooling (pkg/tex/scene/shader corpus scans) |

## Scene viewer

```sh
cargo run --release -p scene-viewer -- ~/.local/share/Steam/steamapps/workshop/content/431960/<id>
cargo run --release -p scene-viewer -- <wallpaper> --screenshot out.png --size 1920x1080 --time 2.5
cargo run --release -p scene-viewer -- <wallpaper> --audio --fps 30 --stats
```

The assets directory is found automatically for common Steam installs; pass `--assets` otherwise.
Set `WPE_DUMP_SHADERS=<dir>` to dump the final GLSL of every compiled shader.

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
