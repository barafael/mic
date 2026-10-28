# Observations

What turned up while building `mic` that the plan didn't expect: surprises, upstream bugs,
measurements, and things to revisit. Newest first within each milestone. Each says whether it's
**open**, **settled** (and where), or **upstream** (to report or follow).

## M11: After the review

- **On PipeWire, every microphone came at half its voice** (settled, mic-media `device`): -6 dB
  since commit 52 put cpal on its PipeWire host. That host offers every input as two channels:
  the defaults hardcode it, and a node without `audio.channels` gets it. A mono microphone then
  comes upmixed into one channel of the two, and mic's average of them halves it: 0.24 where
  0.41 is heard. mic asks PipeWire for one channel instead (cpal requests what it's asked,
  unchecked), and PipeWire mixes down what isn't mono. The box's `audio-devices` caught it, at
  0.24 against the 0.25 wanted: commit 52 hadn't been through it, the box being the other
  session's then.
- **PipeWire's devices were listed by their names** (settled, mic-media `device`). Only
  `alsa_input…`/`…_output…` counted, so the box's `microphone` and `speaker`, and any source
  made by hand (a loopback, an echo canceller), weren't offered. Now by what cpal says: a
  source is an input with a description of its own (an application's stream has none, "unknown"),
  a sink a duplex, which is a speaker and not a microphone.
- **femtovg's wgpu renderer rebuilds its pipelines every frame** (upstream, to report). This is
  what the window's cost since M6 was (docs/review-0.1.0.md). femtovg 0.27.0's `flush` ends by
  dropping every cached pipeline that flush didn't use
  (`pipeline_cache.retain(|_, p| replace(&mut p.accessed, false))`). Slint flushes more than once
  a frame, a layer each, so each flush throws away what the others need, and every frame builds
  them again: naga validates and writes SPIR-V, then Vulkan compiles. Same box, same call of
  two, the same tree: alice's mic took **41 %** of a core stock, **10 %** with the pipelines
  kept. Found with `cargo testbox drive alice profile`: nearly all the main thread's samples were
  in naga, under `create_render_pipeline`.
- **perf works in a rootless pod, from a container of its own** (settled). The host's
  `perf_event_paranoid` is 2: user space only, `--user-callchains`, or the callchain's kernel
  part is refused. Podman's seccomp lets `perf_event_open` through with CAP_PERFMON, and
  attaching takes CAP_SYS_PTRACE (Yama at 1). Not in the window's container: gnome-keyring-daemon
  finds those capabilities, fails to drop them (libcap-ng's -5, the supplementary groups), and
  aborts, and with it the client. So the `profiler` sidecar shares the pod's PID namespace.
  DWARF callchains are shallow: two or three frames, sometimes enough.
- **`cargo testbox down` leaves the builder pod** (observed). Another tree's builder kept
  building that tree into its own target/testbox-bin, while this tree's clients ran an older
  binary from theirs. `podman pod rm -f mic-testbox-builder`, then `up`.
- **The load average can't tell a busy host from a busy box** (settled). Room-40 loads the host
  itself, so `Checks::measured` counts what others used: /proc/stat's busy time less the box's
  pods' cgroups. A pod's cgroup, not a container's, since a restarted container's starts from
  nothing.

## M10: Release

- **0.1.0 reviewed as a product** (open): a full-scenario run of the box and exploratory sessions
  in its windows, written up in [review-0.1.0.md](review-0.1.0.md): no settings screen, raised
  hands off-screen invisible, the chat opening on its oldest, the window's cost doubled since M6,
  and what the box would want to measure and show better.
- **Only Linux's packaging has run** (open, commit 50). The tarball and the AppImage's AppDir
  build here and run (`mic --version`, libvpx carried along); packing the AppImage runs
  appimagetool, which this session wasn't allowed to run from a download. The dmg and the zip,
  the Windows registration and the macOS bundle wait for the release workflow's first run: the
  repository has no remote yet, so CI has never run (as since M1).
- **mic links PipeWire on Linux** (settled, commit 50): Wayland's screen sharing came with
  libpipewire-0.3 as a library mic is linked to, so it doesn't start without one. Desktops have
  it; the README says what the packages want of the system: glibc 2.39 (Ubuntu 24.04's, where
  CI builds), ALSA, fontconfig and freetype, PipeWire, and Vulkan for drawing on the GPU.
- **On macOS a link isn't handed over** (open, commit 50). The bundle's Info.plist makes mic
  what opens `mic://`, but macOS gives the link as an Apple Event, which winit doesn't pass on:
  mic opens, and the invite is pasted. On Windows a link opened while mic runs opens a second
  mic, as only Unix has the hand-over's socket; a named pipe would do it there.
- **The packages aren't signed** (open, commit 50): macOS's is signed ad hoc, Windows's not at
  all, so Gatekeeper and SmartScreen warn on first start. Signing wants certificates the
  project doesn't have.

## M9: Recording

- **A recording is the call as it went, not a file a participant** (settled, after commit 49).
  The first version wrote each participant's camera and voice as they were sent, a file each,
  never encoded again: faithful, and unlike any recording people watch. It now composes one
  picture, as Meet's and Zoom's do: a shared screen, else the active speaker (while someone
  speaks and for 3 s after), else a grid of up to nine, those with a picture first. Its sound is
  every voice decoded, mixed and encoded again, and who's speaking is worked out from that mix,
  this participant included, which the room's speaker leaves out. It takes what the window
  decodes anyway, so it watches no more cameras than the window does: one not shown is a
  colour in the grid. (user's suggestion)
- **The grid's tiles have no names** (open): the composed picture draws no text, so a tile
  without a camera is its participant's colour alone. Names want a font in the core.
- **moq-mux's exporter reads a broadcast, so a recording makes one** (settled, commit 49).
  mic's broadcasts are encrypted with a catalog of their own, which the exporter can't read;
  a recording puts its frames onto a broadcast of the process's own origin, announced (or the
  exporter finds nothing: "unroutable"), with a hang catalog of VP9 and Opus. Its catalog and
  tracks must be finished, not dropped, or the export ends on "json: dropped" before it writes
  out what it holds. Frames given before the exporter subscribes are lost: a fraction of a
  second at the start.

## M8: Rooms

- **Forty in a room fit the budget** (settled, commit 48). Over three minutes with the six
  windows and 34 of the crowd: alice's mic at 58-65 % of a core and 285 MiB (one alone, 33 %
  and 137 MiB), the six windows 3-3.4 cores together, the crowd of 34 about a core in one
  process, the GPU 18-20 % busy (the host's, its desktop included), and the relay sending 29
  Mbit/s, against §5.8's 130 for 40: each window watches eight cameras at 500 kbit/s and one
  voice. (`target/testbox-record/room-40/room-40.txt`) It measures the host as it is: with a
  game running on the desktop, the same room took twice the CPU (alice 132 %, the baseline
  66 %) and the GPU was 98 % busy, past the budget. Its baseline says when the host is loaded.
- **Over the WebSocket fallback, a client sees about ten others** (upstream, commit 48). The
  relay's WebSocket sessions take qmux's default of 100 bidirectional streams (raw TCP gets
  1000), not the QUIC side's `--quic-max-streams`, and a moq-lite subscription is a stream: mic
  subscribes to some seven tracks of each other participant (catalog, checkpoints, profile,
  voice, chat, reactions, screen, and a camera if shown). In room-40, alice on the fallback saw
  10 of 39 within a minute. To report: the relay's qmux limit, configurable as QUIC's is.
  Fewer tracks a participant (chat and reactions on one, the screen only while shared) would
  raise mic's ceiling, not remove it.
- **A WebSocket win sticks for the process's life** (upstream, commit 48). moq-tokio remembers
  each relay where the WebSocket won the race, in a process-wide set that never empties, and
  gives QUIC no head start there again: TCP then tends to win every dial, though UDP works
  again. The box restarts alice after the `network` phase. To report: forget the win when QUIC
  later succeeds, or after a while.

- **Under loss, a voice clicks** (open, commit 46). With 10 % of alice's packets lost and 80 ms
  added, her tone at bob stays over a hundred times louder than any other frequency, but only
  30-70 % of what he hears by power: the rest is clicks. A packet lost within a voice group
  holds up the rest of the group until QUIC sends it again, some 200-260 ms here. bob's stream
  sits near its 120 ms cap (90-110 ms queued in steady state, as nothing drains it), so it
  runs dry, playing silence, then overflows with the burst, which is dropped. A larger cap
  didn't help, nor did the camera off (it isn't congestion). The answer is a jitter buffer
  that aims at a target, conceals what's late (Opus PLC) and catches up, rather than a cap;
  smaller groups would cost checkpoint size (§5.7). Video loses nothing visible at 30 %:
  QUIC's resends arrive within its 2 s.
- **Blocking UDP blocks DNS** (settled, commit 47): the WebSocket fallback couldn't resolve the
  relay either. The `network` phase blocks UDP but port 53.
- **The box's relay never served the fallback** (settled, commit 47). TCP 4443 was plain HTTP,
  so every connect raced a `wss://` dial that failed. The relay serves the fallback over TLS
  there now, and plain HTTP on 4080.
- **Rootless netem works** (settled, commit 46): a NET_ADMIN sidecar, over its pod's network
  namespace alone, adds netem's qdisc, and the host loads `sch_netem` when first asked.

- **A screen is a track, not a broadcast of its own** (settled, commit 45). §5.6 has a
  `screen.hang` broadcast; a track of the participant's broadcast needs no second catalog,
  epoch or checkpoint signer, and a subscriber is told it's shared by the signed profile. The
  track is subscribed to from the first share on: without pictures it costs nothing. The
  encoder runs whenever it's shared, watched or not: demand-gating it (§5.7) waits for the
  encoder to ask the track whether it's used.
- **HD pictures came out in BT.709 on the GPU and BT.601 on the CPU** (settled, commit 45).
  moq-video's renderer guesses a matrix from a picture's height when nothing says, BT.709 above
  576 lines, while mic-render's CPU path, and every picture mic makes, are BT.601. Cameras are
  360 lines, so it only showed with screens: a screen's colours differed with and without a
  GPU, and the box's colour check missed it. mic-render now tags every picture BT.601, and a
  capture in BT.709 is converted.
- **Edits during a box run break its build check** (settled, by practice). The builder compiles
  from the working tree, so `box`'s "a second build is a no-op" fails on a half-edited tree.
  Edit between runs, not during them.
- **Emoji need a font** (settled, commit 44): the client image has Noto Color Emoji, which
  Slint's renderers draw in colour. A desktop without one shows boxes for reactions.

- **Dropping a camera mid-group rejected its sender** (settled, commit 43). A camera no longer
  watched is unsubscribed wherever its group stands. The frames read of that group can't be
  confirmed without the rest, which the checkpoints' chain needs (D18), so after the 2 s grace
  the whole broadcast was rejected as a forgery. The subscriber now lets go of what it read
  and didn't confirm when it drops the track on purpose (`Verification::abandon`). A track
  that ends any other way still counts against its sender.
- **Watch channels must be marked seen** (settled, commit 43). A `watch::Receiver` whose
  `changed()` fires and is then only `borrow`ed stays changed, and a `select!` on it spins.
  The roster's readers `mark_unchanged` after each change.
- **moq-net reuses a lingering track on resubscribe** (upstream, noted). Re-subscribing within
  moq-net's linger window gets the dropped subscription's producer back, its newest group again
  from its start. mic reads it as a new group: the abandoned frames are forgotten.

- **The crowd's media isn't cached on disk** (settled, commit 42). Encoding two seconds of a
  pattern takes well under a second, so each crowd participant encodes its own at start and
  loops it; `cargo testbox media` and `target/testbox-media/` would only add a step to keep fresh.
  The loop repeats frame numbers every two seconds, which no check counts on.

## The window split up, its animations recorded, fifteen at once

- **No animation set from `init` ever ran** (settled). A tile's fade and growth in, the chat's
  slide, the menus' pop, the banners' fade, the hand badge and the unread dot all set their flag in
  `init`, which lands before the first frame: Slint shows the end at once. Recorded frame by frame
  (below), a newcomer appeared whole. They set it from a 16 ms `Timer` now (`ui/README.md`).
- **A newcomer landed on the tiles making room for it** (settled): it appeared in its place while
  the others still moved out of it. It fades in once they've moved (a move's delay); one leaving
  goes at once.
- **Opening the chat made the tiles jump** (settled): the grid took any change of its own width
  for the window being resized, which it follows at once. The room says when the window is
  resized; the chat's narrowing animates. A screen coming or going moves the grid itself (to the
  strip, or back), and tiles placed within it flew in from beyond the window: those follow at
  once, while the stage fades in.
- **Under the software renderer the reactions were blank** (settled). Asking for Noto Color Emoji
  by name gives a renderer that draws no colour emoji nothing to draw; `Theme.emoji-font` is empty
  there (main.rs sets it when wgpu isn't had), and fallback shows what it can.
- **`app.slint` was 1,660 lines** (settled): split as Slint's examples are (a theme global,
  `widgets/`, `pages/`, the room's parts in `room/`, `overlays/`), the room's grid, bar and menus
  components of their own. Every component is exported and loads alone in the preview, with a
  `…Preview` of sample data where it wants one; `tests/ui.rs` holds both. The invite now starts
  under the leftmost tile (or the stage's edge), not the window's.
- **Fifteen windows on one desktop** (settled, by hand): joined through each one's harness, with
  `--ephemeral` (a hidden harness flag: a device secret in memory, so no keyring fills with test
  identities). Nine tiles and "+6", a shared screen beside a strip of them, hands and reactions on
  the small tiles, all as the box's room phase has them. A harness socket's path has to fit a
  Unix socket's (108 bytes): under `$XDG_RUNTIME_DIR`, not a deep scratch directory. The debug
  test relay costs six cores under fifteen; run it `--release`.
- **Recording a window's animation** (settled): in a headless kwin, `ffmpeg -f x11grab
  -window_id <id>` grabs one window at 30 fps (the root is black there); every few frames side by
  side show a transition. `slint-viewer --screenshot` renders a preview without a window.
- **The WebSocket fallback, by hand** (open, the box has it): the client falls back only when UDP
  is blocked, which on a desktop takes root's firewall or a network namespace, and the test relay
  serves no WebSocket. The box's network phase blocks UDP in alice's pod and checks she's heard.

## Window polish after M8, on a real desktop

Found by driving five windows on KDE Plasma 6 (Wayland, GPU renderer), after the headless review;
three of the headless findings (square video corners, a lost first click, blank emoji) were the
software renderer's or the virtual session's.

- **Screenshare never worked on Wayland** (settled). moq-video captured through X11 only, and
  XWayland's root can't be read (`GetImage` fails at once); the picker listed only XWayland's
  windows. moq-video's `pipewire` feature takes the desktop's portal: on Wayland the window offers
  one choice and the desktop's own dialog asks which screen. The box runs X11, so it never saw it.
  The builder image needs `pipewire` for the bindings.
- **A capture that ended was never told** (settled). mic-media's capture logged its end and
  stopped; the share stayed on, the button lit, the others' stage frozen. The room now copies
  each capture's pictures through a channel of its own and stops the share, for everyone, when it
  ends (`a_share_whose_capture_ends_stops_for_everyone`); the window says so when it didn't ask.
- **Starting a share waited on the desktop inside the room's task** (settled). The capture opens
  only once the portal's dialog is answered, which can take as long as the user takes, and the
  room's task waited: no roster, no commands, and leaving timed out. It opens on a blocking
  thread now, the share is answered once it's open, and a stop meanwhile cancels it
  (`a_share_waiting_on_the_desktop_leaves_the_room_free_and_can_be_stopped`). The window says to
  choose in the desktop's dialog, with Cancel: KDE opens it behind mic's window when it likes,
  as no parent window reaches the portal (moq-video passes none).
- **Stop never stopped a Wayland share** (settled). The capture's loop saw a stop only while
  waiting for a frame, and PipeWire's next frame was always ready while scaling a 4K one (debug
  build) took longer than a frame: the room hung joining the capture. The stop is asked before
  each frame now.
- **The second share never reached the desktop** (settled). ashpd keeps one D-Bus connection for
  the process, whose tasks run on the runtime that made it; each capture had a runtime of its
  own, dropped when it stopped, so the next portal call went out on a dead connection. Every
  capture runs on one runtime for the process.
- **A share resumed the last screen without asking** (settled, on a fork). moq-video replays
  the portal's restore token and asks the desktop to remember the choice, with no way to turn it
  off in 0.1.16; mic's fork adds `Config::restore`. Every moq crate comes from that fork's one commit
  (`[patch.crates-io]`; patching moq-video alone made two of moq-net), away from the plan's pinned
  crates.io releases, as the user chose, until moq-dev/moq releases the fixes. moq-dev/moq's `main` keeps the token per
  `portal::Selection`, so a new share asks there already: the fork's commit goes once mic takes
  the next release.
- **When the desktop stopped a share, the capture never ended, and a later stop segfaulted**
  (settled, on a fork; upstream moq-dev/moq#6939). The producer's node goes and the stream only
  pauses, which moq-video doesn't take for an end; its buffers are freed, and a lease still out
  on one was queued back into libpipewire. The fork ends the capture when the node goes and
  retires leases on removed buffers. Seen with KDE's stop button and `pw-cli destroy <node>`.
- **KDE's dialog named mic "Konsole"** (settled). An unsandboxed app is known to the portal by
  the scope it runs in; mic registers as `io.github.barafael.mic` (the portal's host registry,
  on the captures' runtime, before the first call), sets the same Wayland app id, and `--register`
  installs `io.github.barafael.mic.desktop` (removing an older `mic.desktop`).
- **Debug builds abort when they exit** (open; release and the box's builds are fine). Slint keeps
  its platform in a thread-local, dropped among the main thread's after wgpu's debug-only
  `snatch` trace thread-local, which it reads: "cannot access a Thread Local Storage value during
  or after destruction", then a core dump. `process::exit` doesn't avoid it (glibc runs the TLS
  destructors); only `_exit` would.
- **The bar and the chat in narrow windows, the shortcuts in the chat field, the reactions picker,
  colour emoji, the join card's jump** (settled): under 600 px a "More" menu holds the invite,
  the hand, the screen, recording and reactions; under 720 px (900 px beside a shared screen) the chat lies
  over the tiles; the shortcuts are caught before the focused field (`capture-key-pressed`), and
  Escape closes a menu, then the chat; the emoji ask for Noto Color Emoji by name, as fallback
  takes a monochrome font first for four of the six; the join hint keeps its space.
- **Driving the real desktop** (settled): `ydotoold` on a socket of its own and `ydotool` for
  input; KWin scripts over D-Bus (`org.kde.kwin.Scripting`) place windows by pid and read the
  pointer, their `print` landing in the user journal up to seconds late, so a pointer move is
  checked and corrected; `spectacle -b -n -f` for screenshots. Pointer acceleration overshoots a
  long relative move, and the top-left corner opens the Overview. A wrong reading clicked on the
  desktop: watching was cheaper than driving, the user clicked share and stop.

## Window polish (between M7 and M8)

- **Slint's software renderer ignores a negative `transform-scale-x`, and doesn't clip children
  to a rounded corner** (settled, upstream to check). The self-view is mirrored in Rust, the
  same under both renderers (`videos::mirrored`); a tile's video has square corners there.
- **`changed` callbacks run after the change they follow** (settled). A flag set in one comes
  too late to animate what changed with it, so the grid animates every move and turns that off
  while the window is resized, rather than on for a roster change.
- **The join card ran off a short window** (settled): at 330 px tall, Join was below the edge.
  Under 480 px the card drops its greeting.
- **Ctrl+D and Ctrl+E stopped working once the invite field had focus** (settled). The
  shortcuts' FocusScope was a zero-size sibling of the room; clicking the read-only invite field
  took the focus, and nothing gave it back. The room is the FocusScope now, and nothing in it
  takes the focus; the invite is copied with a button. The box didn't see it: its checks never
  touch the field. (by hand, `ui-polish` worktree)
- **At the minimum width the Leave button was off the window** (settled). The invite field took
  the row's width first. The controls are a bar of round buttons now, which fits 360 px.
- **Closing the window never left the room** (settled). `main` made the leaving timeout outside
  the runtime, and its timer panicked ("there is no reactor running") once the event loop ended:
  every window close, and every SIGTERM, dropped the session instead of leaving. The others saw
  the last frames go without a checkpoint and, 2 s later, "Someone in this room publishes what
  they can't sign", until the relay's Gone cleared it. The box's checks leave through the
  harness, so none closed a window. A test now keeps a clean leave free of rejections
  (`one_who_leaves_mid_sentence_leaves_nothing_unsigned`). A crash still shows that warning for
  a few seconds, as the checkpoints fail closed by design. (by hand, `ui-polish` worktree)
- **Running the window by hand without touching the desktop** (settled):
  `kwin_wayland --virtual --xwayland` gives a headless Xwayland that xdotool and `import` drive;
  on the desktop's own Xwayland, xdotool can't warp the pointer. Under it wgpu stops presenting
  after the first frames (Fifo, as on Xvnc), so `VK_ICD_FILENAMES=/nonexistent.json` sends the
  window to the software renderer. A relay comes from `cargo run -p mic-testrelay --example
  serve`, which prints `--relay … --fingerprint …`; a second window on one machine wants its own
  identity, `--keyring <service>`. Input in the virtual session went dead twice after a window
  closed; a fresh kwin brought it back. (`ui-polish` worktree)

## M7: Invites, version check, site

- **A link joins at once, leaving the call it's in** (settled, after M7). A link handed to a
  running mic left its room and joined the link's, unmuted with the camera on, without asking: a
  page that sets `location.href = 'mic://join/…'` (as the invite page does) with the browser's
  "always open" on moved a user between rooms. Now a link never joins by itself. Out of a call
  it's filled in, under the last name, for Join (Enter); in one, the window asks whether to leave
  for it (Enter, Escape), and joins as the call stood: muted, camera off. The `links` phase
  checks both. (review of M7)
- **The review of M7** (settled): the link socket was bound before it was made private, fell back
  to a shared /tmp without `$XDG_RUNTIME_DIR`, took over another mic's live socket, and read
  without a timeout; `--register` wrote Exec without the relay a link needs to start mic, could
  wipe an unreadable mimeapps.list, and named an AppImage's temporary mount; the invite page's
  button showed though `hidden`; version.json could be written empty; the version check ran once,
  racing the box's site. All fixed in the review's commit.

- **xdg-open's generic mode reads `Exec` naively** (settled). It takes a quoted path's quotes for
  part of the command, finds none, and falls back to a browser; and it looks for a scheme's
  handler only with a `$DISPLAY`. mic's desktop file quotes its path only when it must.
  (commit 41)
- **Opening a link needs a name** (settled, revisit): it's filled in under the last name the
  app joined with, or with none for one to be typed. A remembered name across starts would
  want a settings file mic doesn't have yet.
- **Links on Windows and macOS** (open): the registry entry and `Info.plist` come with the
  installers (M10), their steps with TESTPLAN (commit 51). The hand-over to a running mic is a
  Unix socket; Windows wants a named pipe.

- **The Pages actions' SHAs come from GitHub's API, read through a summarizer** (open). Confirm
  them on the first run of `site.yml`: a wrong pin fails loudly, not silently.
- **No version check by default** (open): without a domain, mic checks only where it's told to
  (`--version-url`, `MIC_VERSION_URL`). `mic_core::version::DEFAULT_URL` takes it once there is
  one. (commit 40)
- **`video` was missing from the phases that lease the clients** (settled): an edit whose text
  rustfmt had reflowed didn't apply, unnoticed, since `video` runs alone anyway. (commit 40)
- **The site has no domain yet** (open). Its pages link relatively, and `Invite::to_https_url`
  takes the host; a CNAME file and the release's download links come with one.
- **The Slint badge is a text link** (open). The royalty-free terms' option (b), the badge on the
  download page, wants Slint's own badge image; mic meets option (a), AboutSlint in the window,
  already (design §5.10).

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
