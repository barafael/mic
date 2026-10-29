# mic

Video calls on [Media over QUIC](https://moq.dev): a native app for Linux, Windows and macOS,
end-to-end encrypted, through a MoQ relay. Invite someone with a link; nobody on the way, the
relay included, sees who's in the room or what's said in it.

Version 0.1.0 is the first release. [CHANGELOG.md](CHANGELOG.md) says what it does, and what it
doesn't yet.

## What it does

- **Rooms by invite.** **New room** makes one; **Copy invite** passes on a `mic://join/…` link,
  which opens mic, or the site's `https://…/j/#…` form, which a browser opens first.
- **End-to-end encrypted, and signed.** Every track is encrypted with the room's secret, which
  only the invite carries. Each participant signs what it sends with a key of its own in the
  room, derived from a device secret in the system's keyring: one posing as another is caught,
  and the window says so.
- **Voice and video:** Opus with echo cancellation and noise suppression; your webcam in VP9,
  up to nine tiles, the active speaker and those who spoke last first. The settings keep your
  name, microphone, speaker, camera and noise suppression between starts.
- **Chat, reactions, a raised hand,** encrypted and signed like the rest.
- **Screen sharing:** a display or a window, on X11 and Wayland, macOS and Windows.
- **Recording** to WebM, as the call went: a shared screen, else the speaker, else a grid, with
  every voice. Everyone in the room sees who records.

The keys: **Ctrl+D** the microphone, **Ctrl+E** the camera, **Ctrl+Alt+C** the chat,
**Ctrl+Alt+H** a hand, **Ctrl+Shift+H** leaves.

## Install

The [releases](https://github.com/barafael/mic/releases/latest) have a package for each
platform. They aren't signed yet, so the first start asks to be let through.

- **Linux:** `mic-<version>-x86_64.AppImage`, made executable, or the tarball. Either wants what
  a desktop has: glibc 2.39 or newer, ALSA, fontconfig and freetype, PipeWire, and Vulkan for
  drawing video on the GPU (without it mic draws in software).
  `mic --register` (or `mic.AppImage --register`) makes mic what opens `mic://` links, with the
  options it's given, such as `--relay`.
- **macOS:** open the dmg and drag mic into Applications. It's signed ad hoc only: the first
  time, right-click it and choose **Open**. It asks for the microphone and the camera; sharing a
  screen asks for screen recording.
- **Windows:** unpack the zip anywhere. SmartScreen warns the first time (**More info**, **Run
  anyway**). `mic.exe --register` makes it what opens `mic://` links, for you alone.

## Use

mic needs a MoQ relay: [moq-relay](https://github.com/moq-dev/moq), run by you or for you. Tell
mic where it is with `--relay https://relay.example/` or `MIC_RELAY`; one with a certificate of
its own CA takes `--ca ca.pem`, a self-signed one `--fingerprint <sha-256>`.

```sh
mic --relay https://relay.example/
```

Type your name, then **New room**, or paste an invite and **Join**. `mic --help` lists the rest:
where recordings go (`--recordings`, else `Videos/mic` in your home), QUIC only
(`--no-websocket`, where the WebSocket fallback over TCP mustn't be used), and the version check
(`--version-url`). `mic --devices` lists the microphones, speakers and cameras it finds.

A relay of your own can let only a room's members in, each publishing only at its own path:
run `mic-admit` beside it, and point `moq-relay --auth-url` at it. mic always shows its proof;
an open relay ignores it.

## Build

Rust's toolchain as [rust-toolchain.toml](rust-toolchain.toml) pins it, and libvpx, which
pkg-config finds (`VPX_STATIC=1` links it statically):

- **Linux:** fontconfig, xkbcommon, ALSA, libvpx and PipeWire's headers, and libclang, which makes
  PipeWire's bindings. On Ubuntu: `libfontconfig1-dev libxkbcommon-dev libasound2-dev libvpx-dev
  libpipewire-0.3-dev clang`.
- **macOS:** `brew install libvpx pkg-config`.
- **Windows:** libvpx from vcpkg (`libvpx:x64-windows-static-md`), as
  [CI](.github/workflows/ci.yml) sets it up.

Two dependencies come from forks until upstream releases what mic needs ([Cargo.toml](Cargo.toml),
`[patch.crates-io]`): the moq crates, for screen capture through the portal, and femtovg, whose
0.28 keeps the window's render pipelines, which 0.27, as Slint takes it, rebuilt every frame.

```sh
cargo build --release -p mic-slint     # target/release/mic
cargo test --workspace
```

`packaging/` makes the release packages, as the [release workflow](.github/workflows/release.yml)
runs it for a tag `v<version>`.

## How it's tested

Unit and integration tests run against `mic-testrelay`, a relay in the test process. The test box
([testbox/README.md](testbox/README.md)) runs mic as people would: six windows and a crowd of
headless participants in Podman pods on an internal network, with a real relay, the host's GPU,
PipeWire devices and shaped networks, driven through a harness and checked from what each window
shows. `cargo testbox up`, then `cargo testbox run`. What only a person can check, on each
platform, is in [TESTPLAN.md](TESTPLAN.md).

## Documents

- [PLAN.md](PLAN.md): the order of the work, milestone by milestone.
- [docs/design.md](docs/design.md): the design, and the decisions behind it.
- [docs/observations.md](docs/observations.md): what turned up while building it, measured.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
`mic` by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any
additional terms or conditions.
