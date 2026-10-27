# video-tiles

The video-tile spike (PLAN.md, commit 22; design §5.10): a Slint window with tiles of
`moq-video` frames, drawn by `moq-video`'s renderer on Slint's own wgpu device, and what each
frame costs. It's outside the workspace, with a lockfile of its own, and goes once `mic-slint`'s
tiles are measured the same way (commit 36).

```sh
cargo run --release -- --source cpu --size 720p --tiles 9 --fps 30 --seconds 20
cargo run --release --features vaapi -- --source vaapi --h264 pattern-720p.h264
```

- `--source cpu`: I420 frames from memory, uploaded and converted on the GPU: what a software
  decoder (libvpx, for VP9 in v1) hands over. Decoding itself isn't in it.
- `--source vaapi --h264 <file>`: an H.264 stream, decoded on VA-API for every tile, and the
  DMA-BUFs it decodes into imported zero-copy. The stream must have access unit delimiters:

  ```sh
  ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 2 -pix_fmt yuv420p -c:v libx264 \
    -profile:v main -bf 0 -g 30 -x264-params aud=1 -f h264 pattern-720p.h264
  ```

It prints once a second, and a total:

| Column | What it is |
|---|---|
| fps drawn, ticks/s | Frames Slint rendered; ticks of the timer that feeds the tiles |
| tick | Wall time to give every tile its next frame: decode (VA-API) and the renderer's upload and YUV conversion, mean/p95 |
| Slint build | Slint's `BeforeRendering` to `AfterRendering`: CPU to record the frame |
| tick to GPU done | From a tick to the GPU finishing the frame Slint drew after it (an empty submission after Slint's, waited on from a thread) |
| process CPU | The process's user and system time per tick, and as a share of one core |
| GPU busy | amdgpu's `gpu_busy_percent`, against two seconds of the window standing still first. It's the whole GPU's, compositor included |

## Measured (2026-10-27)

AMD Ryzen AI MAX+ 395, Radeon 8060S (RADV, Mesa 26.2.5), KDE Plasma on Wayland; a 1280x720
window, 9 tiles, 30 fps, 10 s each; Slint 1.18.1 with `renderer-femtovg-wgpu`, `moq-video`
0.1.16.

| Source | Tick | Slint build | Tick to GPU done | Process CPU | GPU busy |
|---|---|---|---|---|---|
| cpu, 720p | 1.76/3.31 ms | 1.47/1.74 ms | 4.38/6.13 ms | 5.9 ms/frame, 18% of a core | 7% (baseline 3%) |
| cpu, 360p | 1.23/2.10 ms | 1.48/2.02 ms | 3.87/5.15 ms | 5.4 ms/frame, 16% | 6% (baseline 5%) |
| cpu, 180p | 1.05/1.57 ms | 1.48/1.96 ms | 3.73/4.94 ms | 5.1 ms/frame, 15% | 5% (baseline 3%) |
| vaapi, 720p | 20.99/35.23 ms | 1.46/2.09 ms | 23.59/38.46 ms | 5.8 ms/frame, 17% | 6% (baseline 3%) |

All four held 30 fps. What it says:

- **Nine tiles are cheap.** Drawing 9 720p tiles at 30 fps costs about a fifth of one core and a
  few percent of this GPU. Most of the CPU is the event loop and Slint, not the tiles: at 180p,
  with a sixteenth of the pixels, it's barely less.
- **Zero-copy works with `WGPUConfiguration::Manual`.** The spike makes its own Vulkan device
  with `VULKAN_EXTERNAL_MEMORY_DMA_BUF` and hands it to Slint; every renderer then logs
  "drawing frames zero-copy … layout=Nv12" and none falls back.
- **VA-API decoding blocks.** A tick decoding 9 streams on the UI thread takes 21 ms of wall
  time, little of it CPU: the decoder waits on the GPU. In `mic`, decoding runs off the UI thread
  and only the newest picture reaches it.
- **`moq-video`'s VA-API H.264 encoder writes no parameter sets here.** Every access unit came
  out 8064 bytes with a NAL header of 0, and `encode::Config::probe` fails with "vaapi emitted
  no usable parameter sets". ffmpeg's `h264_vaapi` encodes correctly on the same driver. `mic`
  encodes with libvpx, so it doesn't block us, but it's worth reporting upstream.

## In the box, on Xvnc

Slint's wgpu renderers configure the surface with `PresentMode::AutoVsync` (Fifo), with no way to
change it. On the box's Xvnc display, which has no vertical blank, that stalls, as `vkcube`
did. Mesa's `MESA_VK_WSI_PRESENT_MODE` overrides it without touching Slint:

| `MESA_VK_WSI_PRESENT_MODE` | fps drawn | Tick to GPU done |
|---|---|---|
| unset | 1.6 | 622/1028 ms |
| `fifo` | 1.4 | 730/1020 ms |
| `mailbox` | 50.9 (30 ticks/s) | 21/36 ms |
| `immediate` | 48.3 (30 ticks/s) | 19/36 ms |

The probe pod, 9 tiles of 720p from the CPU, RADV. More frames are drawn than ticked because
Slint redraws on its own as well. The client image has no fonts, and Slint panics without one
(fontique: `NoMatch`), so this run pointed fontconfig at one through the bind mount.
