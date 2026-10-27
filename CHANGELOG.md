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
- Cameras in VP9; up to nine tiles show, the active speaker and those who spoke last first, and
  only their cameras are received. A camera this client can't decode is said on its tile.
- Mute (Ctrl+D), camera off (Ctrl+E), raise a hand (Ctrl+Alt+H), six reactions, and chat
  (Ctrl+Alt+C), all encrypted and signed like the rest.
- Screen sharing: a display or a window, X11 or Wayland (the desktop portal) on Linux,
  ScreenCaptureKit on macOS, Windows.Graphics.Capture on Windows; shown on a stage beside the
  tiles.
- Recording to WebM, the call as it went: a shared screen, else the active speaker, else a grid,
  with every voice. Everyone in the room sees who records (D7). Into `Videos/mic` unless
  `--recordings` says elsewhere.
- QUIC to the relay, and a WebSocket over TCP where UDP is blocked; `--no-websocket` turns that
  off (D9).
- A notice when a newer version is out, read from the site's `version.json`
  (`--version-url`).

### Known limits

- No camera device yet: only a synthetic one, in the test box.
- On Windows, a link opened while mic runs opens a second window; on macOS, a link opens mic but
  isn't joined, as the window toolkit doesn't hand it over yet: paste it.
- Over the WebSocket fallback, a client sees about ten others: the relay's stream limit there.
- Under heavy loss, a voice clicks: there's no jitter buffer with loss concealment yet.
- Unsigned packages: Windows and macOS warn on first start.

See [docs/observations.md](docs/observations.md) for these and more, with what's measured.
