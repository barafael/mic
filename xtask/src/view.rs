//! The clients' displays, from the host: screenshots (testbox/view/shot), `cargo testbox vnc` in a
//! window (testbox/view/viewer) and `cargo testbox web` as a page for a browser
//! (testbox/view/web). They're of testbox/view's workspace, which only these commands build.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::podman::{DISPLAYS, Pod, TestBox};
use crate::process;

const MANIFEST: &str = "testbox/view/Cargo.toml";

/// The container a display is in: the probe's, or a windowed client's (from M4).
pub fn container(display: &'static str) -> String {
    match display {
        "probe" => Pod::Probe.container("probe"),
        client => Pod::Client(client).container("client"),
    }
}

/// `<name>=<socket>` of each running display, as the tools take them.
pub fn displays(the_box: &TestBox) -> Vec<String> {
    DISPLAYS
        .into_iter()
        .filter(|display| the_box.running(&container(display)))
        .map(|display| format!("{display}={}", the_box.view_socket(display).display()))
        .collect()
}

/// Builds the host tool `package` of testbox/view, for speed (frames are copied by hand), and
/// returns its binary.
pub fn tool(repo: &Path, package: &str) -> Result<PathBuf> {
    let mut build = Command::new(env!("CARGO"));
    build.current_dir(repo).args([
        "build",
        "--quiet",
        "--release",
        "--locked",
        "--manifest-path",
        MANIFEST,
        "-p",
        package,
    ]);
    process::run(build)?;
    Ok(repo.join("testbox/view/target/release").join(package))
}

/// Each of `displays` as it is now, into `<directory>/<label>-<name>.png`.
pub fn shot(tool: &Path, directory: &Path, label: &str, displays: &[String]) -> Result<()> {
    let mut shot = Command::new(tool);
    shot.arg(directory).arg(label).args(displays);
    process::run(shot)
}

/// Shows each running client's display in a window. `interactive` lets the window's keys and
/// pointer through to them; without it they only show, as a check's input and the viewer's
/// would race.
pub fn vnc(the_box: &TestBox, interactive: bool) -> Result<()> {
    viewer(the_box, "testbox-viewer", interactive)
}

/// Serves each running client's display as a web page on loopback; it prints the URL to open.
/// `interactive` as for [`vnc`].
pub fn web(the_box: &TestBox, interactive: bool) -> Result<()> {
    viewer(the_box, "testbox-web", interactive)
}

fn viewer(the_box: &TestBox, package: &str, interactive: bool) -> Result<()> {
    let displays = displays(the_box);
    if displays.is_empty() {
        return Err(Error::Failed(
            "no display runs: `cargo testbox up` first".to_owned(),
        ));
    }
    let input = if interactive { "on" } else { "off" };
    for display in DISPLAYS
        .into_iter()
        .filter(|display| the_box.running(&container(display)))
    {
        let set = the_box.exec(
            &container(display),
            &["/opt/mic/with-display", "input", input],
        )?;
        if !set.status.success() {
            return Err(Error::Failed(format!(
                "{display}'s input: {}",
                String::from_utf8_lossy(&set.stderr)
            )));
        }
    }
    let mut run = Command::new(tool(the_box.repo(), package)?);
    if interactive {
        run.arg("--interactive");
    }
    run.args(displays);
    process::run(run)
}
