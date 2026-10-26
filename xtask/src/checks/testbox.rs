//! The `box` phase: the box itself. Its network reaches nothing outside, an unchanged box starts
//! without building an image, and the builder's binaries run in the client image.

use crate::builder;
use crate::error::{Error, Result};
use crate::images;
use crate::podman::{NETWORK, Pod, TestBox, podman};
use crate::process;

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
    Ok(())
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
