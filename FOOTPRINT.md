# Footprint

What `penguind` costs while it shows a wallpaper, measured on 50 wallpapers from a real library: 24 scenes, 20 videos and 6 web wallpapers, picked at random within each type, each measured with a 60 fps and a 30 fps frame rate cap. Wallpapers are described by what they contain, not by name.

## Setup

- CPU: AMD Ryzen 7 5700X (8 cores, 16 threads); 32 GB RAM
- GPU: AMD Radeon RX 6700 XT (Navi 22), Mesa 26.2.5, amdgpu
- Display: one 1920x1080 output at 180 Hz; KDE Plasma 6.8.1 (KWin, Wayland); Linux 7.2.11 (zen)
- Penguin Engine 0.1.0 (`penguin-engine-git` r79), run by its systemd user service; mpv 0.41.0
- Settings: frame rate cap 60 (first run) or 30 (second run), audio off, no rules, particles 100%, fill auto

## Method

For each wallpaper: `penguinctl set`, then a restart of the service, so each one is measured in a fresh process. Then 8 s to settle and a 20 s window, measuring the `penguind` process:

- **CPU %**: CPU time over the window, in percent of one core (all threads; 100 = one core busy).
- **RAM**: proportional set size (PSS) at the end of the window; shared libraries count in part.
- **VRAM**: the process's video memory, from the amdgpu driver's per-process counters (`/proc/<pid>/fdinfo`).
- **GPU %**: time the GPU spent on the process's graphics work over the window, from the same counters. **Decode %**: the same for the video decode engine (VCN).
- **FPS**: frames presented, averaged over the window.
- **Load s**: how long the service took to restart until its first frame was shown, with the shader cache already warm (a first load compiles shaders and takes longer).

With no wallpaper running, the whole GPU was 1.1% (first run) and 1.9% (second run) busy, from the compositor and other programs; that is not part of the GPU % below.

## Summary

Median / 90th percentile / maximum per type; for FPS, median / minimum.

| Type | Cap | CPU % | RAM (PSS) MB | VRAM MB | GPU % | FPS | Load s |
|---|---|---|---|---|---|---|---|
| scene (24) | 60 | 2.4 / 5.6 / 23.7 | 220 / 592 / 1643 | 559 / 1792 / 2613 | 25.6 / 73.3 / 99.4 | 60 / 8 | 0.5 / 1.1 / 1.3 |
| scene (24) | 30 | 1.4 / 5.4 / 16.2 | 218 / 592 / 1637 | 559 / 1792 / 2613 | 13.4 / 56.7 / 99.4 | 30 / 8 | 0.5 / 1.2 / 1.5 |
| video (20) | 60 | 4.0 / 4.8 / 12.8 | 266 / 670 / 1099 | 426 / 894 / 894 | 8.6 / 56.2 / 65.3 | 60 / 60 | 0.5 / 0.6 / 0.8 |
| video (20) | 30 | 3.2 / 3.9 / 12 | 268 / 681 / 1108 | 426 / 894 / 894 | 7.1 / 39.9 / 47.3 | 30 / 30 | 0.5 / 0.6 / 0.8 |
| web (6) | 60 | 8.6 / 33.5 / 33.5 | 296 / 775 / 775 | 533 / 695 / 695 | 9.6 / 12.5 / 12.5 | 60 / 60 | 0.8 / 1.1 / 1.1 |
| web (6) | 30 | 7.8 / 20.8 / 20.8 | 295 / 770 / 770 | 536 / 695 / 695 | 6.5 / 8.3 / 8.3 | 30 / 30 | 0.8 / 1.1 / 1.1 |

At 60 fps, across all 50: median CPU 3.8% of a core, RAM 266 MB, VRAM 525 MB, GPU 13.4%; 45 of 50 held the cap.

At 30 fps, across all 50: median CPU 2.3% of a core, RAM 268 MB, VRAM 532 MB, GPU 8%; 48 of 50 held the cap.

## 60 fps against 30 fps

Medians at 60 → 30 fps, and how much less the whole sample used at 30 fps (the sum over the wallpapers of that type).

| Type | CPU % | GPU % | Decode % | Total CPU | Total GPU | Total decode |
|---|---|---|---|---|---|---|
| scene | 2.4 → 1.4 | 25.6 → 13.4 | – | -34% | -32% | +0% |
| video | 4 → 3.2 | 8.6 → 7.1 | 15.3 → 15.3 | -18% | -23% | -10% |
| web | 8.6 → 7.8 | 9.6 → 6.5 | – | -22% | -31% | – |

- Scenes gain the most: their median GPU time halves (26% → 13%) and median CPU drops from 2.4% to 1.4% of a core. The two that ran under 30 fps at the 60 cap (8 and 17 fps) cost the same at 30: their limit is the GPU or the video decoder, not the cap. The ones that ran at 30, 36 and 45 fps now hold 30.
- Video wallpapers gain less. Their videos are still decoded at their own frame rate, so decoder time stays about the same for videos of 30 fps or less, and drops only from 68–74% to 55–60% for the 4K 60 fps ones; their GPU time drops from 56–65% to 40–47%. 1080p videos save about one point of GPU and of CPU.
- Web wallpapers save about a third of their GPU time (median 10% → 7%). CPU drops little, except for the busiest page (34% → 18% of a core).
- RAM and VRAM do not depend on the cap: they stayed within 15 MB and 30 MB of the 60 fps run.

## Observations at 60 fps

- Five scenes stayed under 60 fps. Two kept the GPU busy almost the whole time (99% and 92%: an 8192x4608 scene with 13 effects at 8 fps, and a 4912x3368 one with 16 effects and particles at 36 fps). Two play two 4K video textures each and kept the video decoder busy the whole window (30 and 17 fps). The fifth, at 45 fps, has 16 scripts and 33 effects and used the most CPU of the scenes (24% of a core).
- Scene GPU time varies most: from 2.5% for a single image to 99%. Image layers alone cost little; effects on large layers cost the most.
- Scenes of one image layer took 240–500 MB of VRAM; the largest scene (8192x4608) took 2.6 GB.
- Video wallpapers are decoded on the GPU (VCN) for H.264. A 1080p video costs 5–9% GPU, a 4K 30 fps one 24%, and 4K 60 fps ones 56–65%. The one MPEG-4 Part 2 video was decoded on the CPU: 13% of a core and no decoder time.
- RAM of video wallpapers grows with the file: the four largest files (146–462 MB) had the highest RAM (504–1099 MB); most others stayed at 170–380 MB.
- Web wallpapers run Servo inside the daemon (68–74 threads against 11 for a plain scene). They cost the most CPU (7–34% of a core) but little GPU (8–13%).
- Every wallpaper loaded in under 1.4 s with a warm shader cache.

## Scene wallpapers

Sorted by GPU time at 60 fps. Size is the scene's own resolution (it is scaled to the output). CPU, GPU, decode and FPS are shown at 60 → 30 fps; RAM, VRAM and load time are from the 60 fps run.

| Scene size | Contents | Package MB | CPU % | RAM MB | VRAM MB | GPU % | Decode % | FPS | Load s |
|---|---|---|---|---|---|---|---|---|---|
| 1723x2394 | 1 image layer | 0.8 | 1.3 → 0.8 | 135 | 238 | 2.5 → 1.1 | – | 60 → 30 | 0.4 |
| 7998x1823 | 1 image layer | 4.5 | 1.3 → 0.8 | 191 | 302 | 3.1 → 1.5 | – | 60 → 30 | 0.4 |
| 1920x1080 | 1 image layer | 30.6 | 1.4 → 0.7 | 380 | 494 | 3.7 → 2.3 | – | 60 → 30 | 0.5 |
| 3840x2160 | 1 image layer, 2 particle systems, bloom | 6 | 2.3 → 1.3 | 162 | 241 | 5.5 → 2.7 | – | 60 → 30 | 0.4 |
| 2400x3600 | 2 image layers, 1 effect | 15.6 | 1.5 → 0.8 | 403 | 558 | 6.6 → 3.2 | – | 60 → 30 | 0.6 |
| 6000x3401 | 1 image layer, 1 effect | 10.3 | 1.4 → 0.8 | 149 | 494 | 8.1 → 4.1 | – | 60 → 30 | 0.3 |
| 3840x2160 | 4 image layers, 21 effects, 2 particle systems, 3 text layers, 3 scripts | 13.9 | 2.6 → 1.4 | 344 | 817 | 12.1 → 6.1 | – | 60 → 30 | 0.6 |
| 3840x2160 | 8 image layers, 10 effects, 10 text layers, 22 scripts, audio-reactive | 16.3 | 5.6 → 2.9 | 227 | 317 | 14.3 → 7 | – | 60 → 30 | 0.6 |
| 1920x1080 | 1 image layer, 1 particle system, 3 scripts, 1 video texture | 56.6 | 6.4 → 5.4 | 360 | 433 | 14.8 → 13.1 | 31 → 30.6 | 60 → 30 | 0.5 |
| 2480x3508 | 1 image layer, 3 effects | 10.8 | 1.7 → 0.8 | 212 | 559 | 20.8 → 10.4 | – | 60 → 30 | 0.6 |
| 3840x2160 | 1 image layer, 4 effects, 1 particle system | 3.4 | 2.5 → 1.4 | 163 | 497 | 21 → 10.5 | – | 60 → 30 | 0.3 |
| 3840x2160 | 6 image layers, 16 effects, 3 text layers, 7 scripts, audio-reactive | 7 | 3.9 → 1.9 | 179 | 501 | 23.6 → 11.8 | – | 60 → 30 | 0.7 |
| 5374x2721 | 1 image layer, 2 effects | 16.2 | 1.5 → 0.8 | 203 | 558 | 27.5 → 13.8 | – | 60 → 30 | 0.4 |
| 3500x5000 | 3 image layers, 3 effects | 95 | 3.1 → 1.6 | 509 | 1332 | 40.6 → 20.1 | – | 60 → 30 | 1.1 |
| 1920x1080 | 13 image layers, 49 effects, 2 particle systems, 2 text layers, audio-reactive | 12 | 4.7 → 2.4 | 202 | 826 | 40.6 → 20.1 | – | 60 → 30 | 0.6 |
| 3838x2160 | 3 image layers, 10 effects, 2 particle systems, audio-reactive | 8.6 | 2.7 → 1.4 | 191 | 564 | 41 → 19.7 | – | 60 → 30 | 0.5 |
| 4094x2160 | 11 image layers, 10 effects, 1 particle system, 3 text layers, 9 scripts, 2 video textures, 4 sounds, audio-reactive | 423.7 | 5.6 → 5.6 | 1643 | 1946 | 41 → 40.6 | 100 → 100 | 29.9 → 29.6 | 1.3 |
| 5032x2857 | 1 image layer, 5 effects | 17.8 | 1.7 → 0.9 | 204 | 559 | 42.4 → 21.3 | – | 60 → 30 | 0.4 |
| 3292x4800 | 2 image layers, 4 effects | 14.9 | 1.7 → 1 | 208 | 559 | 47.2 → 23.7 | – | 60 → 30 | 0.4 |
| 3840x2160 | 2 image layers, 1 effect, 1 script, 2 video textures | 59.7 | 4.8 → 4.8 | 584 | 1792 | 56.8 → 56.7 | 100 → 100 | 16.6 → 15.7 | 0.6 |
| 5000x2000 | 26 image layers, 75 effects, 6 scripts | 96.2 | 5 → 2.5 | 592 | 1335 | 62 → 30.9 | – | 60 → 30 | 1.1 |
| 3840x2160 | 7 image layers, 33 effects, 5 particle systems, 5 text layers, 16 scripts, 1 sound, audio-reactive | 20.3 | 23.7 → 16.2 | 320 | 580 | 73.3 → 52.2 | – | 45.4 → 30 | 0.7 |
| 4912x3368 | 3 image layers, 16 effects, 1 particle system | 10.4 | 2.2 → 1.7 | 228 | 823 | 92.4 → 75.9 | – | 36.2 → 30 | 0.5 |
| 8192x4608 | 3 image layers, 13 effects, 1 particle system | 142 | 0.5 → 0.5 | 680 | 2613 | 99.4 → 99.4 | – | 8 → 8 | 1.3 |

## Video wallpapers

Sorted by GPU time at 60 fps. Bitrate is the whole file's. CPU, GPU, decode and FPS are shown at 60 → 30 fps; RAM, VRAM and load time are from the 60 fps run.

| Codec | Resolution | Video FPS | Bitrate Mb/s | File MB | Sound | CPU % | RAM MB | VRAM MB | GPU % | Decode % | FPS | Load s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| h264 | 1440x1080 | 30 | 2.2 | 96.1 | yes | 2.8 → 2.1 | 352 | 389 | 4.8 → 3.6 | 6.3 → 6.3 | 60 → 30 | 0.6 |
| h264 | 1920x1080 | 24 | 4.5 | 63.9 | yes | 2.6 → 1.9 | 301 | 425 | 5 → 3.7 | 6.2 → 6.2 | 60 → 30 | 0.6 |
| h264 | 1080x1728 | 30 | 4.1 | 6.1 | yes | 2.8 → 2.2 | 181 | 415 | 5.1 → 3.9 | 7.1 → 7 | 60 → 30 | 0.5 |
| h264 | 1920x1080 | 30 | 4.2 | 5.1 | no | 2.8 → 2.2 | 177 | 425 | 5.5 → 4.4 | 8 → 7.6 | 60 → 30 | 0.5 |
| h264 | 1920x1080 | 30 | 20.9 | 40 | yes | 2.9 → 2.3 | 253 | 400 | 5.7 → 4.5 | 9 → 8.4 | 60 → 30 | 0.5 |
| h264 | 1280x720 | 60 | 1.3 | 9 | yes | 3.8 → 3 | 172 | 334 | 5.8 → 4.4 | 10.2 → 10.3 | 60 → 30 | 0.5 |
| h264 | 1080x1080 | 60 | 4.5 | 36.8 | yes | 4 → 3.2 | 240 | 364 | 6.1 → 4.8 | 11.3 → 11.3 | 60 → 30 | 0.5 |
| h264 | 2000x1500 | 30 | 10.8 | 461.9 | yes | 2.7 → 2.1 | 1099 | 457 | 6.7 → 5.5 | 10.5 → 9.4 | 60 → 30 | 0.8 |
| mpeg4 | 1200x1200 | 60 | 14.8 | 262.1 | yes | 12.8 → 12 | 765 | 343 | 7.6 → 6.1 | – | 60 → 30 | 0.6 |
| h264 | 1920x1080 | 60 | 13.3 | 7.1 | no | 4 → 3.3 | 187 | 426 | 8.6 → 7 | 15.2 → 15.2 | 60 → 30 | 0.5 |
| h264 | 1920x1080 | 60 | 13.7 | 11.2 | no | 4.1 → 3.3 | 215 | 426 | 8.7 → 7.2 | 15.4 → 15.4 | 60 → 30 | 0.5 |
| h264 | 2244x1008 | 59 | 14 | 34.3 | yes | 3.9 → 3.1 | 243 | 412 | 9.3 → 7.7 | 15.9 → 15.7 | 60 → 30 | 0.5 |
| h264 | 2560x1440 | 60 | 151.7 | 145.5 | yes | 4.8 → 3.9 | 504 | 510 | 14.7 → 11.9 | 34.2 → 34.2 | 60 → 30 | 0.6 |
| h264 | 3168x2160 | 25 | 11.7 | 214.7 | yes | 2.6 → 2 | 670 | 843 | 16.8 → 15.2 | 20.5 → 19.6 | 60 → 30 | 0.7 |
| h264 | 1888x2160 | 60 | 9.3 | 22.1 | no | 4 → 3.2 | 241 | 612 | 22.3 → 17.1 | 29.8 → 26.5 | 60 → 30 | 0.5 |
| h264 | 3840x2160 | 30 | 16.6 | 29.7 | yes | 3 → 2.2 | 301 | 843 | 23.5 → 21.7 | 29 → 29.2 | 60 → 30 | 0.3 |
| h264 | 2560x1348 | 120 | 34.5 | 82.6 | yes | 6.2 → 5.3 | 381 | 531 | 24.2 → 21.1 | 45.4 → 43.8 | 60 → 30 | 0.6 |
| h264 | 3840x2160 | 60 | 11.3 | 16.7 | no | 4 → 3.2 | 290 | 894 | 56.2 → 39.9 | 68.1 → 55.3 | 60 → 30 | 0.6 |
| h264 | 3840x2160 | 60 | 14 | 8.9 | no | 4.1 → 3.4 | 263 | 894 | 63.9 → 43.4 | 72.4 → 57.5 | 60 → 30 | 0.5 |
| h264 | 3840x2160 | 60 | 9.7 | 6.2 | no | 4.2 → 3.3 | 268 | 894 | 65.3 → 47.3 | 73.9 → 59.8 | 60 → 30 | 0.5 |

## Web wallpapers

Sorted by CPU time at 60 fps. Contents are what the page's scripts use. CPU, GPU, decode and FPS are shown at 60 → 30 fps; RAM, VRAM and load time are from the 60 fps run.

| Contents | Files MB | CPU % | RAM MB | VRAM MB | GPU % | FPS | Load s |
|---|---|---|---|---|---|---|---|
| WebGL, video | 5.9 | 7.3 → 6.4 | 275 | 547 | 8.9 → 6.6 | 60 → 30 | 1.1 |
| WebGL, video | 9.5 | 8 → 6.9 | 290 | 519 | 8.3 → 6 | 60 → 30 | 0.8 |
| WebGL | 11.2 | 8.1 → 7.1 | 303 | 517 | 8.3 → 6 | 60 → 30 | 0.8 |
| WebGL, video | 17.1 | 9.1 → 8.5 | 317 | 569 | 10.9 → 8.3 | 60 → 30 | 1.1 |
| 2D canvas, video, audio listener | 18.3 | 20.5 → 20.8 | 228 | 413 | 10.2 → 7.3 | 60 → 30 | 0.8 |
| WebGL | 18 | 33.5 → 17.8 | 775 | 695 | 12.5 → 6.4 | 60 → 30 | 0.8 |
