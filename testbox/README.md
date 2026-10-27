# testbox

Checks `mic` in a box of Podman pods on your machine, on an internal network: nothing in it
reaches anything outside. Every upstream is pinned (`xtask/src/pins.rs`), and images take network
only while they build. [PLAN.md](../PLAN.md) says what the box is growing into.

`cargo testbox --help` is the reference for the commands; `cargo testbox` is a Cargo alias
(`.cargo/config.toml`) of the `xtask` package.

## What is in the box

```mermaid
flowchart LR
    subgraph host["Host: AMD GPU, /dev/dri/renderD128"]
        xtask["cargo testbox"]
        bin[("target/testbox-bin")]
        view[("target/testbox-view<br/>display sockets")]
        run[("target/testbox-run<br/>harness sockets")]
    end
    subgraph default["podman's default network"]
        builder["pod mic-testbox-builder<br/>cargo, mold; target and registry volumes"]
    end
    subgraph net["Network mic-testbox: internal, no egress"]
        subgraph prelay["pod mic-testbox-relay"]
            keys["keys<br/>init: the box's CA"]
            relay["moq-relay<br/>QUIC and HTTP on 4443"]
        end
        subgraph ptools["pod mic-testbox-tools"]
            tools["tools<br/>probes, moq's clock"]
        end
        subgraph pprobe["pod mic-testbox-probe"]
            probe["probe<br/>with-display: Xvnc<br/>probe window: wgpu"]
        end
        subgraph palice["pods mic-testbox-alice, -bob"]
            alice["client<br/>with-display: Xvnc<br/>mic: Slint, wgpu<br/>harness, keyring"]
        end
        ca[("volume mic-testbox-keys")]
    end
    xtask -- "podman exec" --> tools
    xtask -- "cargo testbox build" --> builder
    builder --> bin
    bin -. "read-only" .-> probe
    bin -. "read-only" .-> alice
    probe --- view
    alice --- view
    view -. "cargo testbox vnc / web, shot" .- xtask
    alice --- run
    run -- "join, act, snapshot" --- xtask
    alice -- "moq, the box's CA" --> relay
    keys --> ca
    ca -. "read-only" .-> relay
    ca -. "read-only" .-> tools
    tools -- "publish, subscribe" --> relay
```

- **A pod per participant or service**, each from `testbox/pods/<pod>.yaml` under
  `podman kube play`. Containers of one pod share a network namespace, so shaping the network of
  one participant takes a pod of its own.
- **Images are tagged by what they're built from**: a hash of everything in `testbox/<image>/`
  and the pins the image takes. `up` builds only images whose tag is missing, so an unchanged
  box starts without a build. `cargo testbox images --prune` removes former states.
- **Every image is Arch Linux**, pinned by digest, with pacman on one day of the Arch Linux
  Archive: its Mesa is close to the host's kernel driver, which the clients' GPU needs.
- **The relay is `moq-relay`** at a release, in place of moq.pro, which the internal network
  can't reach. It's built from upstream's repository with its `Cargo.lock`, as is moq-tokio's
  `clock` example, the stock publisher and subscriber the `relay` phase uses.
- **`mic` isn't built into an image.** `cargo testbox build` compiles what the box runs in the
  builder pod, a long-lived container whose target directory and cargo registry are volumes,
  linking with mold, in the `testbox` profile (optimized, incremental, no LTO). Each binary goes
  into `target/testbox-bin` only when it changed, by a rename, and client pods mount that
  directory read-only. The builder fetches crates, so it's on podman's default network, outside
  the box; `down` leaves it running, `down --wipe` removes it and its caches.
- **Clients are `mic` participants**, a pod each (`alice`, `bob`): the window
  (crates/mic-slint) on a display of its own, with the host's GPU, trusting the relay by the
  box's CA. A check drives each through its harness (crates/mic-harness), on a socket in
  `target/testbox-run/<client>/` (the owner's only): it joins a fresh room, acts as a click
  would, and reads the room's state and what the window shows of it; or it types into the
  window and clicks it with `xdotool`, as a person would (xtask's `checks::window`).
- **Each client's device secret is in GNOME Keyring**, on a D-Bus session of the client's own
  (`testbox/client/client-session`), as on a desktop. The keyring lives under a home in
  `target/testbox-run/<client>/home`, so a restarted client is the same participant.
- **alice's and bob's voices are tones of their own** (`--tone`, from xtask's `podman::tone`),
  and their speakers analyzers the harness answers `heard` from (crates/mic-harness,
  `synthetic`), so phases measure exactly what each sent.
- **carol uses devices**: PipeWire and WirePlumber run in every client (`client-session`), with
  the box's speaker, whose monitor a check records, and microphone, fed by a sink of its own a
  check plays into (`box-devices.conf`). mic reaches them through cpal and ALSA.
- **Windows present in mailbox mode** (`MESA_VK_WSI_PRESENT_MODE`): Slint presents with Fifo,
  which waits for a vertical blank Xvnc never has (spikes/video-tiles: under 2 fps).
- **A display shows what runs on it**: `with-display` runs a program on TigerVNC's Xvnc, served on
  a Unix socket only, in `target/testbox-view/<client>/` (the owner's only), with the viewer's
  input and the clipboard off. `cargo testbox vnc` shows the displays in a window, `cargo testbox
  web` as a page on loopback; `--interactive` lets keys and pointer through, where they race a
  running check's. `test --screenshots` keeps each display after each check in the phase's
  record. The probe pod's display shows the probe window, for the `box` phase. These tools are
  a workspace of their own in `testbox/view/`, Linux only, carried over from beep's testbox.
- **Clients use the host's GPU**: each client pod gets the render node, `/dev/dri/renderD128`
  (rendering and video only, no mode setting), and the client image carries Mesa's userspace
  drivers: RADV for Vulkan, radeonsi for GL and VA-API. Without a GPU, `up --no-gpu` leaves the
  node out, and Mesa falls back to llvmpipe and lavapipe; checks of hardware paths are skipped
  then, and say so. Xvnc has no vertical blank: what renders there must present in mailbox or
  immediate mode, as FIFO stalls.
- **The box has its own CA.** The relay pod's `keys` init container makes it and the relay's
  certificate once, in the volume `mic-testbox-keys`; clients trust the CA. `down --wipe`
  removes it.

## What the host needs

- For the GPU: `/dev/dri/renderD128` readable and writable by your user (mode 0666, or the
  `render` group), and an AMD GPU for VA-API; otherwise `up --no-gpu`.
- Linux with Podman, rootless: your user needs a subuid and subgid range (`/etc/subuid`,
  `/etc/subgid`). Netavark makes the internal bridge, so the kernel's `bridge` and `veth` modules
  must load.
- Network while the images build, and room for them: the moq image builds the relay from
  source, a few minutes the first time.
- Rust, as for `mic` itself.

## Running a phase

```sh
cargo testbox up
cargo testbox test box
```

`up` builds what's missing (images, then what the box runs), plays the pods, and returns once
the box says it's ready: the relay answers, the probe has drawn, and every client answers `Ping`
on its harness. No fixed sleeps.

Each check prints `check <what>: passed` or `failed: <why>` (or `skipped: <why>`, for a check
that doesn't apply to this box); `test` fails if any failed. The last check of every phase is
that no container crashed, or stopped. The phase's record is in `target/testbox-record/<phase>/`:
every container's log, each line with its time, and all of them merged in time order in
`timeline.log`; after a failed check, each client's harness snapshot (`<nn>-<check>-<client>.json`)
and each display (`.png`), screenshots asked for or not. `cargo testbox down` stops the pods.

Phases don't need a fresh box: each check uses rooms and broadcasts of its own. So:

- **`cargo testbox dev <phase>`** is the inner loop: the box up if it isn't, what it runs built,
  only the pods restarted whose binary changed, then the phase. It says how long that
  took: from an unchanged box, `dev relay` takes about 7 s.
- **`cargo testbox run [<phase>...]`** runs phases (all if none) in rounds (`--rounds n`), up to
  `--jobs n` at once, their lines prefixed with their names, and sums up (phases that drive the
  clients take turns, and one that disrupts the box, as `roster` restarts the relay, runs
  alone); `--fresh` restarts
  every client before each round. Each run's records are in `target/testbox-record/run/<run>/`.
- **`down --wipe`** is for when something is wrong with the box itself.

`up` keeps its flags in `target/testbox-pods/box.settings`, and whatever plays pods again later
(`dev`, `run --fresh`) plays the same box: after `up --no-gpu`, still without the GPU.

| Phase | What it checks |
|---|---|
| `box` | The network is internal; a pod resolves no outside name and opens no outside connection; an unchanged box starts without a build; a second build of an unchanged tree is a no-op; a binary from the builder runs in the client image; the probe window renders on RADV (llvmpipe with `--no-gpu`); a client's display, through its socket, shows what it draws; VA-API decodes VP9; every binary from the builder finds its libraries in the client image |
| `relay` | Frames pass through the relay between moq's own publisher and subscriber; a client without the box's CA is refused. If this fails, the box is broken, not `mic` |
| `roster` | alice and bob join a room and see each other; their windows show the room's tiles, in the roster's order, as the harness's `ui` section says and a screenshot of each shows; a new name reaches the other; bob leaves and alice sees him go, and his window shows the join screen; bob joins again by typing into his window; after the relay restarts, both are connected again and see each other once; a restarted alice has the same key, her device secret kept in her keyring; an honest room rejects nobody |
| `impersonation` | The forger (testbox/forger), a member with the invite, run in the tools pod, speaking an 880 Hz tone and showing a camera of its own (pattern 9): at alice's path with a catalog it signed itself, it stands for no one; at its own path with frames no checkpoint covers, it's rejected within the 2 s grace, after bob heard it and decoded its camera (frames play before they're confirmed), and before he hears only alice and shows its camera no more; bob sees alice once |
| `video` | alice and bob send pattern cameras (ids 1 and 2), carol claims to decode no video (`--decoders none`): each of alice and bob decodes the other's camera, its pattern names the right sender, and the frame rate holds at 70 % of 15 fps; a screenshot of each window shows the other's camera in its colour and not its own, and carol's shows neither, saying on each tile that it can't show their video; then a crowd of six with cameras (mic-crowd, in the tools pod) fills alice's window to nine tiles, seven with video, and what drawing them costs, and alice's CPU, go in the record (`render.txt`); at least 70 % of the pictures must be drawn. It measures, so it runs alone |
| `audio-devices` | carol on PipeWire devices, alice on her tone: what carol's speaker plays (recorded from its monitor) holds alice's tone; a tone played into carol's microphone reaches alice; with carol's speaker linked into her microphone (pw-link), which a recording shows carries alice's tone, alice doesn't hear herself back: the echo is cancelled; with a speech-like voice over noise in carol's microphone, each noise suppressor is measured by what alice hears (in the record, `noise-suppression.txt`), and RNNoise must raise the voice over the noise by 8 dB and keep it. It measures, so it runs alone |
| `audio` | alice and bob each send a tone of their own (alice 440 Hz, bob 660 Hz) and hear the other's, never their own, through their harnesses' analyzers; muted, alice is silence on bob's end and her tile in his window says so, and unmuted she's heard again; bob mutes and unmutes with his window's button (xdotool); once bob leaves, alice hears him no more. It measures, so it runs alone |
| `e2ee` | What a stranger on the relay sees (its HTTP interface: what's announced, a track's latest group): no room id, participant key or name, only ciphertext; a stranger who learned the room's prefix reads its profiles by their real names (the forger's `eavesdrop`): under the room's secret they open, under another every frame fails to authenticate |
