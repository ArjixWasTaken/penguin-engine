# TODO

## Performance

- **Skip video frames above the frame rate cap.** Try mpv's `vd-lavc-skipframe=nonref` with hardware
  decoding; measure decode time on 60 fps videos at a 30 fps cap.
- **Optimized videos.** Re-encode videos larger than the displays showing them (or faster than the
  frame rate cap) to the display's size and the cap, with the hardware encoder (x264 as fallback).
  Keep the sound and the loop; cache the result under `~/.cache/penguin-engine/optimized/`, never
  touch the original. A setting, a background queue and progress in the app.
- **Effects at screen size.** Render each layer's effect buffers at the size the layer covers on the
  output (never above the layer's own size), while shaders keep seeing the original resolution.
  A setting, on by default once screenshot comparisons across the library show no visible
  difference; re-measure the scenes in `FOOTPRINT.md`.
- **Oversized textures.** Downscale textures larger than needed, or compress them at load time, with
  the result cached.

## Features

- **Playlists.** In the daemon first, then the playlist UI in the app.
- **Window rules elsewhere.** `wlr-foreign-toplevel` for compositors other than KWin.
- **App icon.** Replace the placeholder.
