//! `cargo testbox drive`: one window of the box driven by hand, a step an invocation, as a check
//! drives it: join a room, act, read what it shows, type into it, take its display, profile it.
//! What an exploratory session did with ad hoc scripts (docs/review-0.1.0.md), repeatable; what
//! it finds can become a check. Shots and profiles go into target/testbox-record/drive/.

use std::time::{SystemTime, UNIX_EPOCH};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{CLIENTS, TestBox};
use crate::view;

#[derive(clap::Subcommand)]
pub enum Action {
    /// Joins a room, typed as a person would into the join screen: the invite's, or a new one's,
    /// and prints the invite, for another window to join.
    Join {
        name: String,
        /// A `mic://` or `https://…/j/#…` invite; a new room if none.
        invite: Option<String>,
    },
    /// Does what a command says, as JSON: `{"command":"set_muted","muted":true}`
    /// (`mic_core::Command`).
    Act { command: String },
    /// Prints what the window shows, its room and its UI, as JSON.
    Snapshot,
    /// Prints what the window decoded of each camera, as JSON.
    Seen,
    /// Leaves its room, as each check does at its end.
    Reset,
    /// Presses keys in the window, as xdotool names them: `Return`, `ctrl+d`, …
    Key { keys: Vec<String> },
    /// Types text into the window, where its keyboard is.
    Type { text: String },
    /// Takes the window's display, into target/testbox-record/drive/.
    Shot,
    /// Samples the window's mic with perf for a while, user space only, and writes where its
    /// threads spent their time into target/testbox-record/drive/.
    Profile {
        #[arg(long, default_value_t = 10)]
        seconds: u32,
    },
}

pub fn drive(the_box: &TestBox, client: &str, action: Action) -> Result<()> {
    let client = CLIENTS
        .into_iter()
        .find(|known| *known == client)
        .ok_or_else(|| Error::Failed(format!("{client} isn't a window: {}", CLIENTS.join(", "))))?;
    let directory = the_box.repo().join("target/testbox-record/drive");
    match action {
        Action::Join { name, invite } => {
            let invite = match invite {
                Some(invite) => invite
                    .parse::<Invite>()
                    .map_err(|error| Error::Failed(format!("the invite: {error}")))?,
                None => Invite::new_room(),
            };
            crate::checks::window::type_join(the_box, client, &name, &invite)?;
            let mut window = Participant::connect(the_box, client)?;
            window.wait_for("in the room", std::time::Duration::from_secs(15), |state| {
                state.room == invite.room.to_string()
            })?;
            println!("{}", invite.to_mic_url());
        }
        Action::Act { command } => {
            let command: Command = serde_json::from_str(&command)
                .map_err(|error| Error::Failed(format!("the command: {error}")))?;
            Participant::connect(the_box, client)?.act(command)?;
        }
        Action::Snapshot => print_json(&Participant::connect(the_box, client)?.snapshot()?)?,
        Action::Seen => print_json(&Participant::connect(the_box, client)?.seen()?)?,
        Action::Reset => Participant::connect(the_box, client)?.reset()?,
        Action::Key { keys } => {
            for key in keys {
                crate::checks::window::press(the_box, client, &key)?;
            }
        }
        Action::Type { text } => crate::checks::window::type_text(the_box, client, &text)?,
        Action::Shot => {
            let tool = view::tool(the_box.repo(), "testbox-shot")?;
            let display = view::displays(the_box)
                .into_iter()
                .filter(|display| display.starts_with(&format!("{client}=")))
                .collect::<Vec<_>>();
            if display.is_empty() {
                return Err(Error::Failed(format!("{client}'s display isn't running")));
            }
            crate::error::create_dir_all(&directory)?;
            let label = now();
            view::shot(&tool, &directory, &label, &display)?;
            println!(
                "{}",
                directory.join(format!("{label}-{client}.png")).display()
            );
        }
        Action::Profile { seconds } => {
            let report = profile(the_box, client, seconds)?;
            crate::error::create_dir_all(&directory)?;
            let file = directory.join(format!("{}-{client}-profile.txt", now()));
            crate::error::write(&file, &report)?;
            print!("{report}");
            println!("{}", file.display());
        }
    }
    Ok(())
}

/// Where `client`'s mic spent `seconds`, by thread, by function, and by what's on the stack
/// (its callchains are shallow, but often deep enough to say who asked): perf
/// attached to the running process from the pod's profiler (testbox/pods/client.yaml),
/// user space only, as the host's perf_event_paranoid of 2 allows; DWARF callchains, as release
/// builds keep no frame pointers.
fn profile(the_box: &TestBox, client: &'static str, seconds: u32) -> Result<String> {
    let script = format!(
        "set -e; pid=$(pgrep -x mic); data=/tmp/drive.perf; \
         perf record --quiet -e task-clock:u -F 199 --call-graph dwarf --user-callchains \
           -p $pid -o $data -- sleep {seconds} 2>/dev/null; \
         echo '## by thread'; \
         perf report -i $data --stdio --no-children --sort comm -g none --percent-limit 1 \
           2>/dev/null | grep -v '^#' | grep .; \
         echo; echo '## by function, the threads together'; \
         perf report -i $data --stdio --no-children --sort dso,sym -g none --percent-limit 0.5 \
           2>/dev/null | grep -v '^#' | grep .; \
         echo; echo '## in how many samples, as a frame or a caller'; \
         perf script -i $data -F ip,sym 2>/dev/null | sed -n 's/^[[:space:]]*[0-9a-f][0-9a-f]* //p' \
           | grep -vF '[unknown]' | sort | uniq -c | sort -rn | head -60; \
         rm -f $data"
    );
    let profiler = crate::podman::Pod::Client(client).container("profiler");
    let ran = the_box.exec(&profiler, &["sh", "-c", &script])?;
    if !ran.status.success() {
        return Err(Error::Failed(format!(
            "perf in {client}: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| Error::Failed(format!("as JSON: {error}")))?;
    println!("{json}");
    Ok(())
}

/// A label that sorts by when: seconds since the epoch.
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
        .to_string()
}
