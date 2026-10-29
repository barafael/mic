# Test plan

What a person checks before a release, on each platform, with the release's own packages: what
the test box can't, as it runs Linux only, in containers, with synthetic devices. Each step says
what to do and what should happen; a release goes out when every step holds, or what doesn't is
in its changelog's known limits.

The box's phases run first (`cargo testbox run`, and `cargo testbox run room-40` on an idle
host): they cover rooms, encryption, impersonation, voice, video, screens, recording, links on
Linux, and networks. This plan is what's left.

## Setup

- A relay both machines reach: `moq-relay` on a host of your own, with a certificate they trust.
- Two machines, one of them the platform under test, the other any; for links and sharing, a
  browser on the one under test.
- The packages from the draft release: the AppImage and the tarball, the dmg, the zip.

Write down, for each step: the platform and its version, the package, and what happened.

## Every platform

1. **First start.** Start mic with `--relay`. The keyring is asked for the device secret: on
   Linux the Secret Service may ask to be unlocked, on macOS the Keychain, on Windows nothing.
   The window shows the join screen; a second start is the same participant (same tile colour in
   another's window).
2. **A call.** Join one room from both machines. Each sees the other's tile, hears the other, and
   not itself; mute (Ctrl+D) is silence at the other end, and the tile says muted.
3. **Devices.** With speakers, not headphones, talk: the other end hears no echo of itself.
   Typing or a fan near the microphone is quieter at the other end than in the room.
4. **Camera.** With a webcam: the other sees it within a few seconds of the join, and your
   self-view shows it mirrored. **Ctrl+E** turns it off: the other sees your name, and the
   webcam's light goes out; again, and it's back. Choose another camera in the settings: it's
   the one sent, and it's still chosen after a restart. Without one: the camera button is off,
   and the tile shows initials.
5. **Screen sharing.** Share a display, then a window: the other sees it on the stage within a
   few seconds, and it stops when you stop. On Linux, once on X11 and once on Wayland, where the
   desktop's portal asks what to share.
6. **Chat, reactions, hand.** Send a message (Ctrl+Alt+C), a reaction, and raise a hand
   (Ctrl+Alt+H): each shows at the other end.
7. **Recording.** Record a minute: speak, share a screen, then both stay silent. The banner says
   who records on both machines while it runs. The file in `Videos/mic` plays in a browser or
   VLC: the speaker, the screen, then a grid, with both voices.
8. **Links.** Register (below), then click an invite on the site's invite page in the browser:
   mic opens with it filled in, and Enter joins. Click another while in a call: mic asks before
   leaving the call.
9. **A newer version.** Start with `--version-url` pointing at a `version.json` that names a
   newer version: the window says so, with where to get it.
10. **UDP blocked.** Block outgoing UDP to the relay with the platform's firewall, and join: it
    works, over the WebSocket fallback. With `--no-websocket`, the join fails and says the
    fallback is off. Unblock UDP afterwards.
11. **A narrow window.** Make the window 400 px wide: the bar's buttons fold into **More**, and
    each still works.
12. **Leaving.** Close the window during a call: the other end sees you go at once, with no
    warning that something wasn't signed.

## Linux

- **AppImage:** `chmod +x`, start it; `./mic-*.AppImage --register`, then the link step: the
  desktop file is `io.github.barafael.mic.desktop` in `~/.local/share/applications`, and it names
  the AppImage itself, not a temporary mount.
- **Tarball:** unpack, `./mic --register`, the link step.
- On GNOME and on KDE, at least one each, X11 and Wayland between them.

## macOS

- **dmg:** drag mic into Applications; the first start needs right-click, **Open**.
- The microphone, camera and screen recording prompts name mic, and what it says it uses them
  for.
- **Links:** clicking an invite opens mic. In 0.1.0 the invite isn't filled in (a known limit):
  paste it.

## Windows

- **zip:** unpack into a folder with a space in its path (`C:\Program Files\mic` or
  `Documents\my apps`), start mic.exe; SmartScreen's warning, then **Run anyway**.
- `mic.exe --register --relay https://relay.example/` writes `HKCU\Software\Classes\mic`; then
  the link step. In 0.1.0, a link opened while mic runs opens a second window (a known limit).

## Results

| Step | Linux (X11) | Linux (Wayland) | macOS | Windows |
|---|---|---|---|---|
| 1. First start | Holds (a) | Starts and keeps its secret (a); window not seen (c) | | |
| 2. A call | Tiles and mute hold; hearing not checked (b) | Not run (c) | | |
| 3. Devices | Not run (d) | Not run (d) | | |
| 4. Camera | No webcam: button off, initials, settings say "No camera" | Not run (c) | | |
| 5. Screen sharing | Fails under Xwayland (e) | Not run on 0.1.0 (c) | | |
| 6. Chat, reactions, hand | Holds | Not run (c) | | |
| 7. Recording | Holds, but no screen and no voices (f) | Not run (c) | | |
| 8. Links | Holds, after a fix (g) | Not run (c) | | |
| 9. A newer version | Holds (h) | Not run (c) | | |
| 10. UDP blocked | Not run (i) | Not run (i) | | |
| 11. A narrow window | Holds | Not run (c) | | |
| 12. Leaving | Holds (j) | Not run (c) | | |
| Platform's own | AppImage and tarball hold (k) | AppImage starts, renders on the GPU | | |

**Linux, 2026-10-29:** CachyOS (rolling), KDE Plasma 6.8.1 on Wayland, Radeon 8060S, the draft's
AppImage and tarball (v0.1.0 at 61d6615), a local test relay, and two windows on one machine with
their own keyring services. Screenshots of the real desktop failed that night (KWin refused every
capture), so the X11 column is mic as an X11 client of a nested `kwin_wayland --virtual`'s
Xwayland, driven with xdotool; the Wayland column is only what the real desktop showed in logs.
This host has no X11 session and no webcam.

- (a) Kept in the Secret Service (KWallet, unlocked, so nothing asked); a second start is the same
  participant, the same tile colour at the other end.
- (b) Both windows on one machine, at night. The host has no default audio source (WirePlumber's
  is unset; `pw-record` fails the same way), so "System default" didn't open, and the window
  doesn't say so: only the settings' level meter stays empty. Choosing the device by name worked.
- (c) To walk with the user once the desktop takes screenshots again; the Wayland portal share
  held on dev builds on 2026-10-28.
- (d) Needs a person in the room.
- (e) Xwayland's root window refuses `GetImage` (BadMatch), as on the real desktop; a real X11
  server is needed. The failure shows as "Your screen share stopped."
- (f) 68 s of VP9 and Opus at 1280×720, the grid with names and the recording badge; both banners
  showed. No screen (e), and the voices are near silence (b). The WebM has no duration in its
  header: players may show no length or seek bar.
- (g) `--register` writes `io.github.barafael.mic.desktop` naming the AppImage itself, and its
  icon. `xdg-open` stood in for the browser: a link while in a call asks first, and "Leave and
  join" joins. The raised hand came along into the new room (and a share or recording would
  have): fixed in bc18b97, in the re-tagged draft. A link with mic not running wasn't tried: `--register` doesn't keep
  `--fingerprint`, which the local relay needs.
- (h) Both notices: a newer version ("Download", "Not now"), and one this version can't join rooms
  with. The second doesn't stop a join.
- (i) Needs root for the firewall; the box's network phase covers the fallback.
- (j) Closed politely, the other end drops it at once, with no warning. A window destroyed from
  outside (`XDestroyWindow`) panics in wgpu (`Surface::configure`), and the other end then logs
  its audio as unsigned, though it shows no warning.
- (k) The first draft's tarball needed `libvpx.so.9` (this host has `.so.12`). The re-tagged
  draft (v0.1.0 at 88769e6) links libvpx statically: its tarball starts with no libvpx in `ldd`,
  holds a call with the AppImage (tiles, the hand), and `./mic --register` writes the desktop
  file naming the unpacked binary, and its icon; a link while in a call asks first. Its AppImage
  starts too.
