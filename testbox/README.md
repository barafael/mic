# testbox

Checks `mic` in a box of Podman pods on your machine, on an internal network: nothing in it
reaches anything outside. Every upstream is pinned (`xtask/src/pins.rs`), and images take network
only while they build. [PLAN.md](../PLAN.md) says what the box is growing into.

`cargo testbox --help` is the reference for the commands; `cargo testbox` is a Cargo alias
(`.cargo/config.toml`) of the `xtask` package.

## What is in the box

```mermaid
flowchart LR
    subgraph host["Host"]
        xtask["cargo testbox"]
        bin[("target/testbox-bin")]
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
        ca[("volume mic-testbox-keys")]
    end
    xtask -- "podman exec" --> tools
    xtask -- "cargo testbox build" --> builder
    builder --> bin
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
- **The box has its own CA.** The relay pod's `keys` init container makes it and the relay's
  certificate once, in the volume `mic-testbox-keys`; clients trust the CA. `down --wipe`
  removes it.

## What the host needs

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

Each check prints `check <what>: passed` or `failed: <why>`; `test` fails if any did. The last
check of every phase is that no container crashed. The phase's record, every container's log, is
in `target/testbox-record/<phase>/`. `cargo testbox down` stops the pods.

| Phase | What it checks |
|---|---|
| `box` | The network is internal; a pod resolves no outside name and opens no outside connection; an unchanged box starts without a build; a second build of an unchanged tree is a no-op; a binary from the builder runs in the client image |
| `relay` | Frames pass through the relay between moq's own publisher and subscriber; a client without the box's CA is refused. If this fails, the box is broken, not `mic` |
