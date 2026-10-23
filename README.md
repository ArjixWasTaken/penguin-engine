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

## Layout

| Path | Purpose |
|---|---|
| `crates/wpe-vfs` | `.pkg` archives and the layered, case-insensitive asset filesystem |
| `crates/wpe-assets` | TEXB textures, scene/project/material/effect JSON models |
| `crates/wpe-shader` | WE shader preprocessor, GLSL to SPIR-V, reflection, cache |
| `crates/wpe-scene` | Scene graph, cameras, per-frame shader values |
| `crates/wpe-render` | wgpu renderer: render graph, passes, effect chains |
| `crates/wpe-runtime` | Embeddable API tying loading and rendering together |
| `apps/scene-viewer` | Windowed scene viewer |
| `tools/wpe-cli` | Debug tooling (`ls`/`extract` pkg, corpus scans) |

## Scene viewer

```sh
cargo run --release -p scene-viewer -- ~/.local/share/Steam/steamapps/workshop/content/431960/<id>
cargo run --release -p scene-viewer -- <wallpaper> --screenshot out.png --size 1920x1080
```

The assets directory is found automatically for common Steam installs; pass `--assets` otherwise.
Currently renders static image layers with a built-in stand-in for `genericimage2`; real
Wallpaper Engine shaders, effects, particles and text are upcoming milestones.

## Tools

```sh
cargo run -p wpe-cli -- ls path/to/scene.pkg
cargo run -p wpe-cli -- extract path/to/scene.pkg out/
cargo run -p wpe-cli -- scan-pkgs ~/.local/share/Steam/steamapps/workshop/content/431960
cargo run -p wpe-cli -- tex2png --pkg path/to/scene.pkg materials/foo.tex out.png
cargo run -p wpe-cli -- scan-tex --assets <WE assets> --workshop <workshop dir>
cargo run -p wpe-cli -- scan-scenes --assets <WE assets> --workshop <workshop dir>
```
