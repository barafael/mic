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
        subgraph palice["pod mic-testbox-alice"]
            alice["client<br/>with-display: Xvnc<br/>probe window: wgpu"]
        end
        ca[("volume mic-testbox-keys")]
    end
    xtask -- "podman exec" --> tools
    xtask -- "cargo testbox build" --> builder
    builder --> bin
    bin -. "read-only" .-> alice
    alice --- view
    view -. "cargo testbox vnc / web, shot" .- xtask
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
- **Each client has a display of its own**: `with-display` runs it on TigerVNC's Xvnc, served on
  a Unix socket only, in `target/testbox-view/<client>/` (the owner's only), with the viewer's
  input and the clipboard off. `cargo testbox vnc` shows the displays in a window, `cargo testbox
  web` as a page on loopback; `--interactive` lets keys and pointer through, where they race a
  running check's. `test --screenshots` keeps each display after each check in the phase's
  record. These tools are a workspace of their own in `testbox/view/`, Linux only, carried over
  from beep's testbox.
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
the box says it's ready: the relay answers, and every client has logged that it's up. No fixed
sleeps.

Each check prints `check <what>: passed` or `failed: <why>` (or `skipped: <why>`, for a check
that doesn't apply to this box); `test` fails if any failed. The last check of every phase is
that no container crashed. The phase's record, every container's log, is in
`target/testbox-record/<phase>/`. `cargo testbox down` stops the pods.

Phases don't need a fresh box: each check uses rooms and broadcasts of its own. So:

- **`cargo testbox dev <phase>`** is the inner loop: the box up if it isn't, what it runs built,
  the clients restarted only if a binary they run changed, then the phase. It says how long that
  took: from an unchanged box, `dev relay` takes about 7 s.
- **`cargo testbox run [<phase>...]`** runs phases (all if none) in rounds (`--rounds n`), up to
  `--jobs n` at once, their lines prefixed with their names, and sums up; `--fresh` restarts
  every client before each round. Each run's records are in `target/testbox-record/run/<run>/`.
- **`down --wipe`** is for when something is wrong with the box itself.

`up` keeps its flags in `target/testbox-pods/box.settings`, and whatever plays pods again later
(`dev`, `run --fresh`) plays the same box: after `up --no-gpu`, still without the GPU.

| Phase | What it checks |
|---|---|
| `box` | The network is internal; a pod resolves no outside name and opens no outside connection; an unchanged box starts without a build; a second build of an unchanged tree is a no-op; a binary from the builder runs in the client image; the probe window renders on RADV (llvmpipe with `--no-gpu`); a client's display, through its socket, shows what it draws; VA-API decodes VP9 |
| `relay` | Frames pass through the relay between moq's own publisher and subscriber; a client without the box's CA is refused. If this fails, the box is broken, not `mic` |
