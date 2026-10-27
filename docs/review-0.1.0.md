# 0.1.0 as a product: a review from the box

A full-scenario run of the box on `main` (67a08d4), every phase one at a time with screenshots
and room-40, then exploratory sessions in the windows: a typed join, the link prompt, a long
chat, a narrow window, a wrong invite, leaving; and the windows' threads, logs and recordings
looked at. What follows is what a person using mic would meet, then what the box would want,
each with what it rests on. Numbers are this host's (32 cores, a Strix Halo GPU), the box's
windows on Xvnc.

The run: all 15 phases passed, but `recording`, whose check was wrong under load (fixed, below).
The host was not idle for the first half: another session ran a relay and 18 windows by hand,
and beep's box ran beside ours (load 40). Correctness held through that; the numbers here are
from after, with the host quiet, where they differ.

## What works well

- **Joining is fast.** A typed name and invite, then Enter: the room with bob in it, and his
  first picture, 0.67 s later. A join with UDP blocked, over the WebSocket fallback, 0.2 s.
- **The window reads well**: the join card, the room's grid and bar, the banners (reconnecting,
  sharing with its **Stop**, recording, a broadcast that doesn't stand), the link prompt, the
  More menu in a narrow window. Nothing overlaps or clips at 960×600 or 460×560.
- **The core holds under strain**: a restarted relay is rejoined, a forger is caught within the
  grace, 30 % loss costs video nothing visible (QUIC resends within 2 s), a room of 40 fits the
  budget (the relay sends 29 Mbit/s of §5.8's 130).
- **The recording is the call as it went**: alice's camera while she speaks, her screen while she
  shares, a grid once nobody speaks, checked frame by frame.

## For people using it

Most important first.

1. **There's no settings screen.** No microphone or speaker to pick (the system's default is
   it), no noise suppression to turn off, no name kept across starts (the name field is empty at
   every start). A call app is judged on its device picker first. *Suggest:* a settings panel
   with devices, a level meter, noise suppression, and the name remembered (a small settings
   file, which observations already asks for).
2. **A raised hand off-screen is invisible.** In a room of twelve, dave raised his hand; in
   frank's window dave is in the "+3", so frank sees nothing. The "+N" tile doesn't say who's
   in it either, and there's no participant list. *Suggest:* a participants panel (names, hands,
   muted, who records), and a raised hand counting as a speaker in which tiles show
   (`view::Shown`).
3. **The chat opens on the oldest messages.** With 14 in it, the panel shows 0 to 8; the newest
   are below, unscrolled to. No times on messages, and a sender's name repeats on each.
4. **Alone in a call, your own camera fills the window.** The moment to pass the invite on is
   the one with the least said about it. *Suggest:* "You're the only one here" with the invite
   and **Copy**, as Meet has, and the self-view small.
5. **A relay blip empties the room.** While reconnecting, everyone else's tile goes, and they
   come back as new tiles: it looks as if everyone left. *Suggest:* keep the tiles, greyed,
   while the room redials.
6. **The version notice can't be acted on.** "mic 99.0.0 is out, and this 0.1.0 can't join rooms
   with it. Download it: http://…" is plain text: the URL isn't a link, the banner can't be
   dismissed, and "can't join rooms with it" is alarming without saying what to do. It also
   takes a line of every screen. *Suggest:* a link that opens the browser, a close button, and
   "Update to keep joining rooms with people on 99.0.0".
7. **A broadcast that doesn't stand says too little.** "2 broadcasts in this room aren't signed
   by who they claim. They aren't shown." names nobody and says nothing to do. *Suggest:* who
   was posed as, and "Someone with this room's invite is posing as them: the invite may have
   leaked; a new room keeps them out", with **New room**.
8. **Failing to reach the relay takes 10 s and talks like a log.** "no session with the relay at
   … within 10s: reconnect timed out after 10s: connect timed out after 10s (…)" *Suggest:*
   "Can't reach the relay at relay.example" after a few seconds, the details behind a click.
9. **Nothing says the call is on the fallback.** Over the WebSocket, a client sees about ten
   others (observations); a person in a room of twelve would see some missing and not know why.
   *Suggest:* a quiet note, "Connected over TCP: UDP seems blocked".
10. **Rooms have no names.** The link prompt can only show the raw link, a recording's file is
    named by the room's id, and two invites can't be told apart. *Suggest:* an optional room
    name in the invite (it's the room's secret's business, not the relay's).
11. **Leaving is silent.** After **Leave**, the join screen comes back with the last invite
    filled in (good for rejoining) but nothing says you left, and there's no **Rejoin**.
12. **The recording's grid has no names**, so a participant without a camera is a coloured block
    (observations, M9). And the composed picture draws no "Recording" mark of its own.
13. **A participant's colour changes from room to room** (per-room keys): the same person is
    pink in one call and teal in the next. Not wrong, but it can't be relied on to recognise
    anyone; names should carry that, which (1) and (2) help.

## Performance

- **The window costs twice what it did at M6.** Seven cameras decoded and nine tiles drawn take
  alice's mic 56 % of a core on the quiet host (M6: 29 %). By thread: the main thread, Slint's
  event loop and the drawing, 48 %; decoding, on the runtime's threads, 7 %. Alone in a room,
  only its own preview, 33 %. Part is the box's: Xvnc has no DRI3, so every presented frame is
  copied through the CPU. Part is likely the window's since M6 (the self-view, rounded and
  animated tiles, a tile's row set on each picture). *Suggest:* measure on a real desktop, and
  profile in the box (below); a frame only when a picture or a property changes.
- **Memory is up a little**: alice in the room of 40 holds 325 MiB (M8: 285).
- **Under load, a shared screen takes about 3 s to reach a recording**: the share's capture,
  its first keyframe at 5 fps, the catalog, the subscription.
- **A voice under 10 % loss** stays dominant but clicks (observations, M8): a jitter buffer.

## Correctness and polish

- **Leaving warns in every log.** Each leave logs moq-net's "track::Producer dropped without
  finish() or abort()" a few times: mic aborts its voice and camera senders, whose tracks then
  drop unfinished. A clean leave finishes them. Harmless, but it hides real warnings.
- **Every start warns of small UDP buffers** (moq-sock wants 8 MiB, the host gives 4): a host
  setting, but a user sees it on every start in a terminal. Logged once at info would do.
- **The window's screenshot lagged its state once under load**: dave's own raised hand was in
  his window's state (the check passed) but not yet drawn. A frame owed under load; worth
  watching, not a bug yet.

## For the test box

1. **Checks that measure should know the host is busy.** This run's video phase measured 94 %
   of a core under load 40, 56 % quiet; room-40's baseline doubled once under a game. A phase
   that measures could record the host's load before and after, and say a measurement is
   unreliable when it was high, rather than pass or fail on it.
2. **Screenshots come after a check, not during it.** Transient UI, the link prompt, a reaction's
   bubble, a "Joining…", is never in the record. A check could take its own shot at the moment
   that matters (as `recording` reads frames, and as these exploratory sessions did).
3. **Snapshots are kept only for failures.** A passing check's harness snapshot (who's speaking,
   hands, what's shown) would explain its screenshot.
4. **No profiler in the box.** perf isn't on the host or in the images; per-thread CPU from
   /proc pointed at the main thread, and that's as far as it went. `perf` in the client image,
   or a sampling profiler in the window behind a flag.
5. **Exploratory driving deserves a tool.** This review drove the windows with ad hoc Python
   over the harness's sockets and xdotool; `cargo testbox drive <client> join|say|shot …` would
   make such sessions repeatable, and a check of each.
6. **`run` takes no `--screenshots`, and a sweep of every phase with them is a loop of `test`s.**
7. **The checks' timing assumptions break under load** (the recording check counted from the
   wrong moment, fixed in 05e6e6d): a check should count from what it caused, not from when it
   finished waiting.
8. **What the box doesn't cover:** real cameras (there's no camera device yet), a real desktop's
   compositor and portal (Wayland sharing was tested by hand), Windows and macOS (TESTPLAN.md),
   long calls (nothing runs past room-40's minutes: leaks over an hour), and a relay that isn't
   ours (moq.pro).

## Next

By the product's measure: a settings screen with devices and a remembered name; a participants
panel with hands; the chat opening on its newest; the "you're alone" state; tiles kept through a
reconnect; the version notice as a link. Then the window's cost on a real desktop, with a
profiler, and the clean leave.
