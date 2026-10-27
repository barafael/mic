//! `cargo testbox build`: compiles what the box runs in the builder pod (testbox/pods/builder.yaml),
//! incrementally, and puts each binary into `target/testbox-bin` only when it changed, by a
//! rename, so a running client keeps the file it started from.

use crate::error::Result;
use crate::images;
use crate::podman::{Pod, TestBox};
use crate::process;

/// Where the binaries land on the host; the clients mount it read-only.
pub const BIN: &str = "target/testbox-bin";

/// A package the box runs, and its binary.
struct Package {
    name: &'static str,
    /// Its Cargo features, comma-separated.
    features: &'static str,
    binary: &'static str,
}

const fn package(name: &'static str) -> Package {
    Package {
        name,
        features: "",
        binary: name,
    }
}

/// What the box runs, by workspace: mic's, the window with its harness among it, and the
/// display tools' (testbox/view), whose `with-display` runs a program on its display.
const WORKSPACES: [(&str, &[Package]); 2] = [
    (
        "Cargo.toml",
        &[
            package("forger"),
            package("mic-crowd"),
            Package {
                name: "mic-slint",
                features: "mic-slint/harness",
                binary: "mic",
            },
            package("probe"),
        ],
    ),
    ("testbox/view/Cargo.toml", &[package("with-display")]),
];

/// The pods that run `binary`, which a change to it restarts.
pub fn pods_running(binary: &str) -> Vec<Pod> {
    match binary {
        "mic" => crate::podman::CLIENTS.map(Pod::Client).to_vec(),
        "probe" => vec![Pod::Probe],
        "with-display" => std::iter::once(Pod::Probe)
            .chain(crate::podman::CLIENTS.map(Pod::Client))
            .collect(),
        _ => Vec::new(),
    }
}

/// What the build script says, on stderr, for a binary it put into place.
const UPDATED: &str = "testbox-builder: updated ";

/// One script for the builder, so a build is one `podman exec`: each workspace's packages, then
/// each binary into place by a rename, and only when it differs, so a client restarts only for a
/// change and a running one keeps the file it started from.
fn script() -> String {
    let mut script = String::from("set -e\n");
    for (manifest, packages) in WORKSPACES {
        let names: String = packages
            .iter()
            .map(|package| format!(" -p {}", package.name))
            .collect();
        let features: Vec<&str> = packages
            .iter()
            .map(|package| package.features)
            .filter(|features| !features.is_empty())
            .collect();
        let features = if features.is_empty() {
            String::new()
        } else {
            format!(" --features {}", features.join(","))
        };
        script.push_str(&format!(
            "cargo build --profile testbox --locked --manifest-path {manifest}{names}{features}\n"
        ));
    }
    for binary in WORKSPACES
        .iter()
        .flat_map(|(_, packages)| packages.iter().map(|package| package.binary))
    {
        script.push_str(&format!(
            "if ! cmp -s /target/testbox/{binary} /out/{binary}; then \
             cp /target/testbox/{binary} /out/.{binary}.new; \
             mv -f /out/.{binary}.new /out/{binary}; \
             echo '{UPDATED}{binary}' >&2; fi\n"
        ));
    }
    script
}

/// What a build did.
pub struct Built {
    /// Whether cargo compiled anything.
    pub compiled: bool,
    /// The binaries that changed in `target/testbox-bin`.
    pub updated: Vec<String>,
}

pub fn build(the_box: &TestBox) -> Result<Built> {
    let repo = the_box.repo();
    let builder = Pod::Builder.container("builder");
    let image = images::find("builder").ok_or("no builder image")?;
    let (tag, _) = image.ensure(repo)?;
    if !the_box.running(&builder) || the_box.image_of(&builder).as_deref() != Some(tag.as_str()) {
        the_box.play(Pod::Builder)?;
    }
    let script = script();
    let mut exec = crate::podman::podman(&["exec", &builder, "bash", "-c", &script]);
    exec.env_remove("CARGO_TERM_COLOR");
    let lines = process::run_capturing_stderr(exec)?;
    let compiled = lines.iter().any(|line| line.contains("Compiling "));
    let updated: Vec<String> = lines
        .iter()
        .filter_map(|line| line.strip_prefix(UPDATED))
        .map(str::to_owned)
        .collect();
    for binary in &updated {
        println!("updated {BIN}/{binary}");
    }
    Ok(Built { compiled, updated })
}
