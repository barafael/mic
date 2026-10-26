//! What a phase leaves behind for looking into it once the box is gone:
//! `target/testbox-record/<phase>/`, with every container's log at the phase's end, and all of
//! them merged in time order (`timeline.log`); after a failed check, each client's harness
//! snapshot and each display (testbox/view/shot); and with `--screenshots`, each display after
//! every check.

use std::path::{Path, PathBuf};

use crate::error::{self, Result};
use crate::harness::Participant;
use crate::podman::{CLIENTS, PREFIX, RECORD, TestBox, Timestamp};
use crate::view;

/// How long a check's name may be in a screenshot's file name.
const LABEL: usize = 60;

pub struct Record {
    directory: PathBuf,
    /// With `--screenshots`.
    shots: Option<Shots>,
    /// The shot tool, once a failure needed it.
    tool: Option<PathBuf>,
    /// How many checks ran, which numbers what's kept of them.
    checked: usize,
}

/// The screenshots: the tool, the displays as it takes them (`<name>=<socket>`), and how many
/// were taken, which numbers them in the order of the checks.
struct Shots {
    tool: PathBuf,
    displays: Vec<String>,
    taken: usize,
}

impl Record {
    /// The record in `target/testbox-record/<name>`, emptied of a former run's; with
    /// `screenshots`, the shot tool built and the running clients' displays found.
    pub fn new(the_box: &TestBox, name: &str, screenshots: bool) -> Result<Self> {
        let directory = the_box.repo().join(RECORD).join(name);
        if directory.exists() {
            std::fs::remove_dir_all(&directory).map_err(|source| error::Error::Io {
                what: directory.display().to_string(),
                source,
            })?;
        }
        error::create_dir_all(&directory)?;
        let shots = if screenshots {
            Some(Shots {
                tool: view::tool(the_box.repo(), "testbox-shot")?,
                displays: view::displays(the_box),
                taken: 0,
            })
        } else {
            None
        };
        Ok(Self {
            directory,
            shots,
            tool: None,
            checked: 0,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// After the check `name`: with `--screenshots`, each display; after a failure, each
    /// display and each client's harness snapshot, screenshots or not. What can't be kept fails
    /// no check: it is said so, and the run goes on.
    pub fn checked(&mut self, the_box: &TestBox, name: &str, failed: bool) {
        self.checked += 1;
        let label = format!("{:02}-{}", self.checked, label(name));
        if let Some(shots) = &mut self.shots
            && !shots.displays.is_empty()
        {
            shots.taken += 1;
            if let Err(error) = view::shot(&shots.tool, &self.directory, &label, &shots.displays) {
                println!("screenshot {label}: {error}");
            }
        } else if failed {
            self.shoot_failure(the_box, &label);
        }
        if failed {
            self.keep_snapshots(the_box, &label);
        }
    }

    /// Each running display, after a failure, without `--screenshots`.
    fn shoot_failure(&mut self, the_box: &TestBox, label: &str) {
        let displays = view::displays(the_box);
        if displays.is_empty() {
            return;
        }
        if self.tool.is_none() {
            match view::tool(the_box.repo(), "testbox-shot") {
                Ok(tool) => self.tool = Some(tool),
                Err(error) => {
                    println!("screenshot {label}: {error}");
                    return;
                }
            }
        }
        if let Some(tool) = &self.tool
            && let Err(error) = view::shot(tool, &self.directory, label, &displays)
        {
            println!("screenshot {label}: {error}");
        }
    }

    /// Each client's harness snapshot, as `<label>-<client>.json`.
    fn keep_snapshots(&self, the_box: &TestBox, label: &str) {
        for client in CLIENTS {
            let snapshot = Participant::connect(the_box, client)
                .and_then(|mut participant| participant.snapshot())
                .map(|snapshot| {
                    serde_json::to_string_pretty(&snapshot)
                        .unwrap_or_else(|error| error.to_string())
                })
                .unwrap_or_else(|error| error.to_string());
            self.write(&format!("{label}-{client}.json"), &snapshot);
        }
    }

    /// Every container's log into the record, each line with its time, and all of them merged
    /// in time order, each line with its container's name, as `timeline.log`. What can't be had
    /// is noted in its file instead.
    pub fn keep_logs(&self, the_box: &TestBox) {
        let mut timeline: Vec<(Timestamp, String)> = Vec::new();
        for container in the_box.containers() {
            let short = container
                .strip_prefix(PREFIX)
                .unwrap_or(&container)
                .to_owned();
            match the_box.log_timestamped(&container) {
                Ok(lines) => {
                    let log: String = lines
                        .iter()
                        .map(|(time, line)| format!("{} {line}\n", time.as_str()))
                        .collect();
                    self.write(&format!("{container}.log"), &log);
                    timeline.extend(
                        lines
                            .into_iter()
                            .map(|(time, line)| (time, format!("{short:<16} {line}"))),
                    );
                }
                Err(error) => self.write(&format!("{container}.log"), &error.to_string()),
            }
        }
        // Stable: lines of one container at one instant keep their order.
        timeline.sort_by(|left, right| left.0.cmp(&right.0));
        let merged: String = timeline
            .iter()
            .map(|(time, line)| format!("{} {line}\n", time.as_str()))
            .collect();
        self.write("timeline.log", &merged);
    }

    fn write(&self, name: &str, contents: &str) {
        let path = self.directory.join(name);
        if let Err(error) = error::write(&path, contents) {
            println!("record: {error}");
        }
    }
}

/// A check's name as part of a file name: lowercase letters and digits, the rest one dash.
fn label(name: &str) -> String {
    let mut label = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            label.push(c.to_ascii_lowercase());
        } else if !label.is_empty() && !label.ends_with('-') {
            label.push('-');
        }
    }
    let mut label: String = label.trim_end_matches('-').chars().take(LABEL).collect();
    while label.ends_with('-') {
        label.pop();
    }
    label
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn a_label_is_lowercase_letters_digits_and_single_dashes() {
        assert_eq!(
            label("A display shows its pixels: 1280x720!"),
            "a-display-shows-its-pixels-1280x720"
        );
    }
}
