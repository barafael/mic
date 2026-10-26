//! `cargo testbox build`: compiles what the box runs in the builder pod (testbox/pods/builder.yaml),
//! incrementally, and puts each binary into `target/testbox-bin` only when it changed, by a
//! rename, so a running client keeps the file it started from.

use crate::error::{Error, Result};
use crate::images;
use crate::podman::{Pod, TestBox};
use crate::process;

/// Where the binaries land on the host; the clients mount it read-only.
pub const BIN: &str = "target/testbox-bin";

/// The packages the box runs, and their binaries.
const BINARIES: [(&str, &str); 1] = [("probe", "probe")];

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
    let mut cargo = vec!["cargo", "build", "--profile", "testbox", "--locked"];
    for (package, _) in BINARIES {
        cargo.extend(["-p", package]);
    }
    let mut exec = crate::podman::podman(&["exec", &builder]);
    exec.args(&cargo);
    let compiled = process::run_watching(exec, "Compiling ")?;
    let mut updated = Vec::new();
    for (_, binary) in BINARIES {
        // Replaced by a rename, and only when it differs: a client restarts only for a change.
        let install = format!(
            "if ! cmp -s /target/testbox/{binary} /out/{binary}; then \
             cp /target/testbox/{binary} /out/.{binary}.new && mv -f /out/.{binary}.new /out/{binary} \
             && echo updated; fi"
        );
        let installed = the_box.exec(&builder, &["bash", "-c", &install])?;
        if !installed.status.success() {
            return Err(Error::Failed(format!(
                "installing {binary}: {}",
                String::from_utf8_lossy(&installed.stderr)
            )));
        }
        if String::from_utf8_lossy(&installed.stdout).contains("updated") {
            println!("updated {BIN}/{binary}");
            updated.push(binary.to_owned());
        }
    }
    Ok(Built { compiled, updated })
}
