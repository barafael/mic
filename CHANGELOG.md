# Changelog

All notable changes to mic. The version's major (its minor before 1.0) is the wire format: a
mic of another can't share a room with this one, and says so when the site announces it.

## 0.1.0

The first release: video calls on [Media over QUIC](https://moq.dev), through a MoQ relay, for
Linux, Windows and macOS.

### Calls

- Rooms by invite: `mic://join/…` links, or the site's `https://…/j/#…` form, made with **New
  room** and passed on with **Copy invite**. A link opened in a browser opens mic; one opened
  during a call asks before leaving it.
- End-to-end encryption on every track, with the room's secret, which only the invite carries:
  the relay sees opaque names and ciphertext, and doesn't learn who's in a room (D17).
- Each participant's broadcast signed with a key of its own in the room, derived from a device
  secret in the system keyring: a member posing as another is caught within two seconds, and
  the window says so (D16, D18).
- Voice in Opus, with echo cancellation (AEC3) and noise suppression (RNNoise).
- Cameras in VP9, from the system's webcam (PipeWire or V4L2 on Linux, AVFoundation on macOS,
  Media Foundation on Windows); turned off, the webcam is let go, its light out. Up to nine
  tiles show, the active speaker, a raised hand and those who spoke last first, and only their
  cameras are received; the "+N" says who else is there. A camera this client can't decode is
  said on its tile.
- Settings, kept between starts: your name, the microphone (with a level meter), the speaker,
  the camera, and noise suppression; `mic --devices` lists what's there.
- A participants panel: everyone in the room, their hands, who's muted, who records.
- Mute (Ctrl+D), camera off (Ctrl+E), raise a hand (Ctrl+Alt+H), six reactions, and chat
  (Ctrl+Alt+C), all encrypted and signed like the rest. The chat opens on its newest, with
  times, a sender's lines together.
- Leave with **Leave** or Ctrl+Shift+H: the join screen says so, and **Rejoin** goes back in.
  Alone in a room, the window offers the invite to pass on.
- Rooms can have a name, which the invite carries (nobody else learns it): the window, a link's
  asking, and a recording's file name show it.
- A relay that restarts is rejoined, everyone's tiles kept, greyed, while it redials.
- Screen sharing: a display or a window, X11 or Wayland (the desktop portal) on Linux,
  ScreenCaptureKit on macOS, Windows.Graphics.Capture on Windows; shown on a stage beside the
  tiles.
- Recording to WebM, the call as it went: a shared screen, else the active speaker, else a grid,
  with every voice, each tile named, and marked as a recording. Everyone in the room sees who
  records (D7). Into `Videos/mic` unless `--recordings` says elsewhere.
- QUIC to the relay, and a WebSocket over TCP where UDP is blocked, which the window says;
  `--no-websocket` turns that off (D9).
- What's wrong is said plainly: a relay that can't be reached, in a few words, the details
  behind them; someone posing as a participant, named, with **New room** (the invite may have
  leaked).
- Admission (§5.11): each client shows a relay a proof that it holds the room's invite, and of
  the one path it may publish at. A relay of your own with `mic-admit` as its auth server
  (`moq-relay --auth-url`) lets only members in, each only at its own path; an open relay
  ignores the proof. The relay sees two keys a participant, derived apart from the ones that
  sign: it can't tell from them who's who in the room.
- A notice when a newer version is out, read from the site's `version.json`
  (`--version-url`), with its download link; Escape or **Not now** puts it away until the next
  start.

### Known limits

- On Windows, a link opened while mic runs opens a second window; on macOS, a link opens mic but
  isn't joined, as the window toolkit doesn't hand it over yet: paste it.
- Over the WebSocket fallback, a client sees about ten others: the relay's stream limit there.
- Under heavy loss, a voice clicks: there's no jitter buffer with loss concealment yet.
- Unsigned packages: Windows and macOS warn on first start.
- A camera that's plugged in during a call is offered from the next join; one there at the join
  can be switched in the settings at any time.
- Admission is tested against the test suite's relay, not yet in the box with a real
  `moq-relay`; moq.pro doesn't take its proof yet (that needs a minting front end). A client a
  relay refuses looks joined for a moment, then hears nothing, and the window doesn't say why.

See [docs/observations.md](docs/observations.md) for these and more, with what's measured.
