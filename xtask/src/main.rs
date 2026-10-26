//! `cargo testbox`, a Cargo alias of this xtask (.cargo/config.toml). testbox/README.md says what
//! the box is and how to use it.

// Every outcome is handled: a discarded `Result` would hide a failed step.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

mod builder;
mod checks;
mod error;
mod images;
mod pins;
mod podman;
mod process;
mod view;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use error::Result;
use podman::TestBox;

/// Checks mic in a box of Podman pods on an internal network: nothing in it reaches anything
/// outside. Every upstream is pinned, and images take network only while they build.
#[derive(Parser)]
#[command(bin_name = "cargo testbox")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Builds the images that are missing, and plays the pods.
    Up,
    /// Compiles what the box runs, in the builder pod, into target/testbox-bin.
    Build,
    /// Runs a phase's checks against the box; its record is in target/testbox-record/<phase>/.
    Test {
        phase: Phase,
        /// Also takes each client's display after each check, into the phase's record.
        #[arg(long)]
        screenshots: bool,
    },
    /// Shows the clients' displays in a window.
    Vnc {
        /// Lets the window's keys and pointer through to the clients, where they race a running
        /// check's.
        #[arg(long)]
        interactive: bool,
    },
    /// Serves the clients' displays as a page on loopback, and prints its address.
    Web {
        /// As for `vnc`.
        #[arg(long)]
        interactive: bool,
    },
    /// Lists the images' current tags, content-hashed.
    Images {
        /// Removes the box's images that no current tag names: former states of the images.
        #[arg(long)]
        prune: bool,
    },
    /// Stops and removes the pods.
    Down {
        /// Also removes their volumes.
        #[arg(long)]
        wipe: bool,
    },
    /// Prints logs: of a container (`<pod>-<container>`), or of every container of a pod.
    Logs { names: Vec<String> },
}

/// `test`'s phases.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Phase {
    /// The box itself: no egress, and no build when nothing changed.
    Box,
    /// Frames pass through the relay between a stock moq publisher and subscriber.
    Relay,
}

impl Phase {
    fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|value| value.get_name().to_owned())
            .unwrap_or_default()
    }
}

fn main() -> ExitCode {
    let repo = repo_root();
    let the_box = TestBox::new(&repo);
    let outcome = match Cli::parse().command {
        Command::Up => the_box.up(),
        Command::Build => builder::build(&the_box).map(|_| ()),
        Command::Test { phase, screenshots } => test(&the_box, phase, screenshots),
        Command::Vnc { interactive } => view::vnc(&the_box, interactive),
        Command::Web { interactive } => view::web(&the_box, interactive),
        Command::Images { prune } => the_box.list_images(prune),
        Command::Down { wipe } => the_box.down(wipe),
        Command::Logs { names } => the_box.logs(&names),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("testbox: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}

fn test(the_box: &TestBox, phase: Phase, screenshots: bool) -> Result<()> {
    let record = checks::Record::new(the_box, &phase.name(), screenshots)?;
    checks::phase(the_box, record, |checks| match phase {
        Phase::Box => checks::testbox(checks, the_box),
        Phase::Relay => checks::relay(checks, the_box),
    })
}
