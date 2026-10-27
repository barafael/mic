# Observations

What turned up while building `mic` that the plan didn't expect: surprises, upstream bugs,
measurements, and things to revisit. Newest first within each milestone. Each says whether it's
**open**, **settled** (and where), or **upstream** (to report or follow).

## M6: Video (VP9)

- **A click at fixed coordinates hit the wrong button** (settled). The new Camera button pushed
  Mute left, and `audio`'s click landed on Camera. Checks now press the room's shortcuts (Ctrl+D,
  Ctrl+E, as Meet has them), which call what the buttons call; no check clicks at coordinates.
  Slint reports Ctrl+D's text as "d" under winit on X11. (commit 38)

- **VP9 on VA-API is deferred** (open, upstream). moq-vaapi has no VP9 decoder (the plan's
  unverified point), and ChromeOS's `cros-codecs`, which has one in Rust, doesn't build here:
  its binding `cros-libva` 0.0.12, from crates.io and from git `main` alike, misses fields
  newer libva headers add (`seg_id_block_size`, `va_reserved8` in
  `VAEncPictureParameterBufferVP9`). Writing a VA-API VP9 decoder (uncompressed header,
  reference frames) is the upstream work the plan named, more than a commit. Software decoding
  is cheap at these sizes (29% of a core with 7 cameras); hardware decoding matters for the
  40-participant rooms of M8. Also: a VA-API decoder outside moq-video couldn't hand mic-render a
  DMA-BUF, since `moq_video::DmaBuf::new` is the crate's own. (commit 37, deferred)

- **Nine tiles in the box cost 29% of a core** (settled, measured 2026-10-27). Alice's mic was
  decoding 7 VP9 cameras at 640x360 and 15 fps, encoding her own, running both voices, and
  drawing 9 tiles. It drew 102 to 110 pictures a second, each in 0.11 to 0.15 ms, with Slint's
  frame at 3.4 to 4.0 ms, on RADV. The spike's 18% was drawing only, at 720p, without decoding.
  (commit 35, the `video` phase's `render.txt`)
- **The tiles draw on the window's own device** (settled): mic makes the Vulkan device and hands
  it to Slint (`WGPUConfiguration::Manual`), asking for DMA-BUF import where there is any, for
  VA-API to come. Without wgpu, the software renderer and mic-render's CPU path. (commit 35)

- **moq-video's renderer needs view formats, which wgpu's GL backend lacks** (upstream, to
  report). On OpenGL (llvmpipe here) its output texture fails validation (`VIEW_FORMATS`). mic
  renders on Vulkan, Metal or DX12, so `mic-render`'s test asks for those; a GL-only machine would
  need the CPU path. (commit 34)
- **The offscreen tile test passes on lavapipe** (settled here, open for CI): run in the client
  image, with no GPU, it draws the pattern as the CPU path has it. CI installs lavapipe and
  requires the test there (`MIC_TEST_GPU=1`), not yet run. (commit 34)

- **One who decodes nothing would leave everyone without video** (settled). With a single codec,
  "the first codec every participant decodes" is none as soon as one client decodes nothing, and
  then nobody sends. Such a participant is now left out of the choice, and told it can't decode,
  as design §5.4 asks of the tile. With two codecs the same rule keeps Jitsi's fallback for those
  who decode some. (commit 33)
- **The box's library check looked only at the client image** (settled). The forger runs in the
  tools pod, of the moq image, and failed to start once it linked libvpx, which showed only as
  forgeries never rejected. `box` now checks the forger in its own image. (commit 33)
- **The camera's codec is chosen at the join, not as the roster changes** (open). With one codec
  it's the same answer; a second codec needs the choice to follow the roster, and the catalog's
  rendition re-signed at a group boundary.
- **No camera device yet** (open). The window sends a camera only with the box's
  `--camera-pattern`; capture (moq-video's, or v4l2/PipeWire) isn't in the plan's M6 list.

- **VP9 decodes through libvpx directly, not moq-video's `vpx` feature** (settled, revisit). The
  feature is only on moq-video's git `main`, and a git dependency on moq-video brings moq-net,
  hang and moq-mux from git too, beside the crates.io ones mic is pinned to (moq-relay 0.17.8 and
  its set): two copies whose types don't meet. mic encodes with libvpx anyway, so both directions
  go through `libvpx-native-sys`, the binding upstream's decoder uses. The decoder moves upstream
  once a moq-video release has it. (commit 32)
- **moq's `main` reads older than its releases** (open, odd). Fetched raw, the workspace on
  `main` names moq-net 0.3.10 and moq-video 0.1.10, where crates.io has 0.3.16 and 0.1.16: a stale
  view, or releases cut from elsewhere. Worth asking upstream before pinning to git.
- **`libvpx-native-sys` is MPL-2.0** (settled): file-level copyleft, allowed for that crate only in
  `deny.toml`. libvpx itself is BSD-3-Clause.
- **libvpx at 150 kbit/s keeps the pattern readable at 180p** (settled): 59 of 60 frames read back
  in the unit test, the rest while rate control settles. (commit 32)
- **libvpx on Windows and macOS CI** (open): vcpkg's static build by `VPX_LIB_DIR`, and Homebrew's
  through pkg-config, written but not run.

## M5: Audio

- **AEC3 starts out cautious** (open). While it hasn't yet found that there's no echo, a quiet
  near-end voice talking over the far end is suppressed: by 16 dB at -30 dBFS for its first 6 s,
  none of it by 20 s. A headset user is quieter for their first seconds in a room while others
  talk. Measured with synthetic signals, not speech. Options: sonora's AEC3 settings, or starting
  with the canceller off until an echo is seen. (design §5.5)
- **AEC3 suppresses a loud near-end tone in double talk** (open). A near-end tone at -6 dBFS
  talking over the far end is taken for echo; one at speech level (-20 dBFS) is kept. A tone isn't
  speech, but double talk wants measuring with speech. (design §5.5)
- **Both noise suppressors take a steady tone for noise** (settled). They're measured with a
  speech-like voice, and the box's synthetic tones go without suppression. (commit 30)
- **RNNoise beats sonora's suppressor** (settled): +11.5 to +12.3 dB against +9.4 to +10.6 dB, and
  keeps the voice whole (0.1 to 0.3 dB against 1.6 to 2.0 dB). Only white noise is measured; babble
  and keyboard noise aren't. (design §5.5)
- **WirePlumber links playback only to sinks** (settled). A virtual source can't be played into, so
  the box's microphone is a loopback fed by a sink of its own. (`box-devices.conf`)
- **PipeWire records a sink's monitor only when asked** (settled): `stream.capture.sink = true`.
  Without it a recording of the speaker is silence. (`audio_devices.rs`)
- **A group per Opus packet would swamp the checkpoints** (settled, revisit). Each group is a
  ~60-byte entry in every checkpoint for 3 s; at 50 groups a second, some 9 KB per checkpoint,
  four times a second. Voices use a group a second instead, at the cost of head-of-line blocking
  under loss, to be measured by the `network` phase. Smaller entries would allow smaller groups.
  (design §5.7)
- **A subscriber coming mid-group gets a second of backlog at once** (settled). A stream that
  queues more than 120 ms drops what comes. (`mic-core` `audio`)
- **Devices must do 48 kHz f32** (open). No resampling yet; true of PipeWire, unverified on
  Windows and macOS.
- **`cargo testbox run` doesn't build** (observed). After changing code, run `dev` or `build`
  first, or round 1 runs the previous binary: the `box` phase's no-op-build check then rebuilds
  mid-run.

## M4: The window

- **Slint presents only with Fifo** (settled). On Xvnc that stalls below 2 fps;
  `MESA_VK_WSI_PRESENT_MODE=mailbox` brings it to 30. (spike, client pods)
- **Slint panics without a font** (settled): fontique `NoMatch`. The client image carries one.
- **moq-video's VA-API H.264 encoder writes no parameter sets on Mesa 26.2.5** (upstream, to
  report). Every access unit is 8064 bytes with a NAL header of 0, and `Config::probe` fails;
  ffmpeg's `h264_vaapi` works on the same driver. Not in mic's path. (spike README)
- **Zero-copy VA-API to Slint works** through `WGPUConfiguration::Manual` with
  `VULKAN_EXTERNAL_MEMORY_DMA_BUF` (settled). VA-API decode blocks its thread, so it stays off the
  UI thread. (spike)
- **9 tiles of 720p cost about 18% of a core** on the Radeon 8060S (settled). (spike README)
- **xdotool needs `windowfocus` without a window manager**, and its `type` takes every argument
  after it as text (settled). (`checks::window`)
- **A window draws a moment after its harness says what it shows** (settled). The screenshot
  check retries within its patience.
- **No default relay** (open). `--relay`/`MIC_RELAY` until moq.pro's public prefix is settled.

## Across milestones

- **CI has never run** (open): there's no remote. Slint, cpal and the keyring on Windows and macOS
  are unverified until it does.
