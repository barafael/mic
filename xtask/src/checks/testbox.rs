//! The `box` phase: the box itself. Its network reaches nothing outside, an unchanged box starts
//! without building an image, the builder's binaries run in the client image, a client renders on
//! the host's GPU (or on llvmpipe without it), and its display shows what it draws.

use crate::builder;
use crate::error::{Error, Result};
use crate::images;
use crate::podman::{NETWORK, Pod, TestBox, podman};
use crate::process;
use crate::view;

use super::Checks;

/// An address outside the box: one of Cloudflare's resolvers, which answers on 443 anywhere.
const OUTSIDE: &str = "1.1.1.1";

pub fn testbox(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    checks.record("the network is internal", || {
        if the_box.network_is_internal()? {
            Ok(())
        } else {
            Err(Error::Failed(format!("{NETWORK} has egress")))
        }
    });
    checks.record("a pod reaches nothing outside", || unreachable(the_box));
    checks.record("an unchanged box starts without a build", || {
        let built = the_box.images()?;
        if built.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!("built {}", built.join(", "))))
        }
    });
    checks.record("a second build of an unchanged tree is a no-op", || {
        builder::build(the_box)?;
        let again = builder::build(the_box)?;
        if again.compiled || !again.updated.is_empty() {
            Err(Error::Failed(format!(
                "compiled: {}, updated: {:?}",
                again.compiled, again.updated
            )))
        } else {
            Ok(())
        }
    });
    checks.record("a binary from the builder runs in the client image", || {
        probe_runs(the_box)
    });
    checks.record(
        "every binary from the builder finds its libraries in the client image",
        || libraries_found(the_box, "client", None),
    );
    // The forger and the crowd a check starts run in the tools pod, of the moq image.
    checks.record(
        "the forger and crowd find their libraries in the tools image",
        || libraries_found(the_box, "moq", Some(&["forger", "mic-crowd"])),
    );
    let client = "probe";
    let gpu = has_gpu(the_box, client)?;
    let expected = if gpu { "RADV" } else { "llvmpipe" };
    checks.record(&format!("the probe window renders on {expected}"), || {
        adapter(the_box, client, expected)
    });
    let directory = checks.record_directory().to_owned();
    checks.record("a display shows what is drawn on it", || {
        display_shows(the_box, &directory)
    });
    if gpu {
        checks.record("VA-API decodes VP9 in the box", || {
            vp9_decode(the_box, client)
        });
    } else {
        checks.skip("VA-API decodes VP9 in the box", "no GPU (`up --no-gpu`)");
    }
    Ok(())
}

/// What the probe window clears its window to (testbox/probe/src/main.rs, `COLOUR`).
const BACKGROUND: [u8; 3] = [0xc0, 0x39, 0x2b];

/// Whether `client`'s pod has the host's render node.
fn has_gpu(the_box: &TestBox, client: &'static str) -> Result<bool> {
    let found = the_box.exec(
        &view::container(client),
        &["test", "-c", crate::podman::RENDER_NODE],
    )?;
    Ok(found.status.success())
}

/// Once the probe window has presented a frame, the adapter it says it got is `expected`'s.
fn adapter(the_box: &TestBox, client: &'static str, expected: &str) -> Result<()> {
    let log = crate::ready::display_up(the_box, client)?;
    let adapter = log
        .lines()
        .find(|line| line.starts_with("probe: adapter "))
        .ok_or("presented, but named no adapter")?;
    if adapter.contains(expected) {
        Ok(())
    } else {
        Err(Error::Failed(format!("{adapter:?}")))
    }
}

/// `vainfo` in `client` lists VP9 profile 0 for decoding (`VAEntrypointVLD`).
fn vp9_decode(the_box: &TestBox, client: &'static str) -> Result<()> {
    let listed = the_box.exec(
        &view::container(client),
        &[
            "vainfo",
            "--display",
            "drm",
            "--device",
            crate::podman::RENDER_NODE,
        ],
    )?;
    let listed = String::from_utf8_lossy(&listed.stdout);
    if listed
        .lines()
        .any(|line| line.contains("VAProfileVP9Profile0") && line.contains("VAEntrypointVLD"))
    {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "vainfo lists no VP9 decoding: {listed}"
        )))
    }
}
/// Its size there.
const DISPLAY: (u32, u32) = (1280, 720);
/// How long a display may take to serve after `up`.
const DISPLAY_START: std::time::Duration = std::time::Duration::from_secs(30);

/// A screenshot of the probe's display, through its socket, is the size the pod gives it,
/// and every pixel is the colour it was set to.
fn display_shows(the_box: &TestBox, directory: &std::path::Path) -> Result<()> {
    let client = "probe";
    let socket = the_box.view_socket(client);
    let deadline = std::time::Instant::now() + DISPLAY_START;
    while !socket.exists() {
        if std::time::Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "no display at {} within {}s",
                socket.display(),
                DISPLAY_START.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let tool = view::tool(the_box.repo(), "testbox-shot")?;
    let display = format!("{client}={}", socket.display());
    view::shot(&tool, directory, "display", &[display])?;
    let path = directory.join(format!("display-{client}.png"));
    let file = std::fs::File::open(&path).map_err(|source| Error::Io {
        what: path.display().to_string(),
        source,
    })?;
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .map_err(|error| Error::Failed(format!("{}: {error}", path.display())))?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| Error::Failed(format!("{}: {error}", path.display())))?;
    if (info.width, info.height) != DISPLAY {
        return Err(Error::Failed(format!(
            "{}x{}, not {}x{}",
            info.width, info.height, DISPLAY.0, DISPLAY.1
        )));
    }
    let channels = info.color_type.samples();
    let off = pixels[..info.buffer_size()]
        .chunks_exact(channels)
        .filter(|pixel| pixel[..3] != BACKGROUND)
        .count();
    if off == 0 {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{off} pixels aren't #{:02x}{:02x}{:02x}",
            BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]
        )))
    }
}

/// The probe from `target/testbox-bin`, in the client image with no network, says it runs
/// (testbox/probe/src/main.rs, `RUNS`).
fn probe_runs(the_box: &TestBox) -> Result<()> {
    let client = images::find("client").ok_or("no client image")?;
    let tag = client.tag(the_box.repo())?;
    let bin = the_box.repo().join(builder::BIN);
    let mut run = podman(&["run", "--rm", "--network", "none", "--volume"]);
    run.arg(format!("{}:/opt/mic:ro", bin.display()))
        .args([tag.as_str(), "/opt/mic/probe"]);
    let said = process::output(run)?;
    if said.starts_with("probe: runs") {
        Ok(())
    } else {
        Err(Error::Failed(format!("it said {said:?}")))
    }
}

/// `ldd` of every binary in `target/testbox-bin`, or those `only` names, in `image`: none names a
/// library it can't find. The builder and the images install their packages apart, and their
/// lists drift as libraries arrive (libvpx, PipeWire, D-Bus).
fn libraries_found(the_box: &TestBox, image: &str, only: Option<&[&str]>) -> Result<()> {
    let image = images::find(image).ok_or("no such image")?;
    let tag = image.tag(the_box.repo())?;
    let bin = the_box.repo().join(builder::BIN);
    let mut binaries: Vec<String> = std::fs::read_dir(&bin)
        .map_err(|source| Error::Io {
            what: bin.display().to_string(),
            source,
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        // The builder's half-written files start with a dot.
        .filter(|name| !name.starts_with('.'))
        .filter(|name| only.is_none_or(|only| only.contains(&name.as_str())))
        .collect();
    binaries.sort();
    if binaries.is_empty() {
        return Err(Error::Failed(
            "no binaries: `cargo testbox build`".to_owned(),
        ));
    }
    let script = binaries
        .iter()
        // Each ldd's status ignored: it fails on a static binary, which misses nothing. A missing
        // library is in what it says.
        .map(|binary| format!("echo '== {binary}'; ldd /opt/mic/{binary} 2>&1 || true"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut run = podman(&["run", "--rm", "--network", "none", "--volume"]);
    run.arg(format!("{}:/opt/mic:ro", bin.display()))
        .args([tag.as_str(), "bash", "-c", &script]);
    let listed = process::output(run)?;
    let mut binary = "";
    let mut missing = Vec::new();
    for line in listed.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            binary = name;
        } else if line.contains("not found") {
            missing.push(format!("{binary}: {}", line.trim()));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(missing.join("; ")))
    }
}

/// From the tools pod: a name doesn't resolve, and a connection to an address doesn't open. Both
/// only count if the pod runs commands at all, or a stopped pod would pass.
fn unreachable(the_box: &TestBox) -> Result<()> {
    let tools = Pod::Tools.container("tools");
    let alive = the_box.exec(&tools, &["true"])?;
    if !alive.status.success() {
        return Err(Error::Failed(format!(
            "{tools} runs no commands: {}",
            String::from_utf8_lossy(&alive.stderr)
        )));
    }
    let resolved = the_box.exec(&tools, &["getent", "hosts", "example.org"])?;
    if resolved.status.success() {
        return Err(Error::Failed(format!(
            "example.org resolves: {}",
            String::from_utf8_lossy(&resolved.stdout).trim()
        )));
    }
    let connect = format!("exec 3<>/dev/tcp/{OUTSIDE}/443");
    let connected = the_box.exec(&tools, &["timeout", "10", "bash", "-c", &connect])?;
    if connected.status.success() {
        return Err(Error::Failed(format!("{OUTSIDE}:443 accepts a connection")));
    }
    Ok(())
}
