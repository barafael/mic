# Observations

What turned up while building `mic` that the plan didn't expect: surprises, upstream bugs,
measurements, and things to revisit. Newest first within each milestone. Each says whether it's
**open**, **settled** (and where), or **upstream** (to report or follow).

## M6: Video (VP9)

(Entries are added as the milestone goes.)

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
