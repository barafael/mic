//! `cargo testbox`, a Cargo alias of this xtask (.cargo/config.toml). testbox/README.md says what
//! the box is and how to use it.

// Every outcome is handled: a discarded `Result` would hide a failed step.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

#[cfg(unix)]
mod builder;
#[cfg(unix)]
mod checks;
#[cfg(unix)]
mod error;
#[cfg(unix)]
mod harness;
#[cfg(unix)]
mod images;
#[cfg(unix)]
mod pins;
#[cfg(unix)]
mod podman;
#[cfg(unix)]
mod process;
#[cfg(unix)]
mod ready;
#[cfg(unix)]
mod run;
#[cfg(unix)]
mod settings;
#[cfg(unix)]
mod view;

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(unix)]
use clap::Parser;

#[cfg(unix)]
use checks::Phase;
#[cfg(unix)]
use podman::TestBox;

/// Checks mic in a box of Podman pods on an internal network: nothing in it reaches anything
/// outside. Every upstream is pinned, and images take network only while they build.
#[cfg(unix)]
#[derive(Parser)]
#[command(bin_name = "cargo testbox")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(clap::Subcommand)]
enum Command {
    /// Builds the images that are missing and what the box runs, and plays the pods.
    Up {
        /// Client pods without the host's GPU: llvmpipe and lavapipe, and software codecs, as
        /// in CI. Phases that measure the hardware paths skip them, and say so.
        #[arg(long)]
        no_gpu: bool,
    },
    /// Compiles what the box runs, in the builder pod, into target/testbox-bin.
    Build,
    /// Runs a phase's checks against the box; its record is in target/testbox-record/<phase>/.
    Test {
        phase: Phase,
        /// Also takes each client's display after each check, into the phase's record.
        #[arg(long)]
        screenshots: bool,
    },
    /// Runs phases on the box in rounds, and sums up; each run's records are in
    /// target/testbox-record/run/<run>/.
    Run {
        /// The phases; all of them if none.
        phases: Vec<Phase>,
        /// How many times each.
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        /// How many phases at once: they use rooms of their own, so they share the box.
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Restarts every client before each round.
        #[arg(long)]
        fresh: bool,
    },
    /// The inner loop: the box up, what it runs built, the clients restarted if that changed,
    /// then the phase; the box stays up.
    Dev { phase: Phase },
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

/// The box is Podman on Linux, its sockets Unix sockets: elsewhere, there's no box.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("cargo testbox: the box runs on Linux only");
    ExitCode::FAILURE
}

#[cfg(unix)]
fn main() -> ExitCode {
    let repo = repo_root();
    let command = Cli::parse().command;
    // `up` brings up the box its flags say, and keeps them; the rest take the box as it was
    // brought up.
    let the_box = match &command {
        Command::Up { no_gpu } => Ok(TestBox::with(&repo, settings::Settings { gpu: !no_gpu })),
        _ => TestBox::new(&repo),
    };
    let the_box = match the_box {
        Ok(the_box) => the_box,
        Err(reason) => {
            eprintln!("testbox: {reason}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match command {
        Command::Up { .. } => the_box.up(),
        Command::Build => builder::build(&the_box).map(|_| ()),
        Command::Test { phase, screenshots } => {
            run::test(&the_box, phase, &phase.name(), screenshots, "")
        }
        Command::Run {
            phases,
            rounds,
            jobs,
            fresh,
        } => run::run(&the_box, &phases, rounds, jobs, fresh),
        Command::Dev { phase } => run::dev(&the_box, phase),
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

#[cfg(unix)]
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}
