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
4. **Camera.** There's no camera device in 0.1.0: the camera button is off, and the tile shows
   initials. (When there is one: the other sees it.)
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
| 1–12 | | | | |
| Platform's own | | | | |
