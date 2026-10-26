//! What a phase leaves behind for looking into it once the box is gone:
//! `target/testbox-record/<phase>/`, with every container's log at the phase's end, and with
//! `--screenshots` each client's display after each check (testbox/view/shot).

use std::path::{Path, PathBuf};

use crate::error::{self, Result};
use crate::podman::{RECORD, TestBox};
use crate::view;

/// How long a check's name may be in a screenshot's file name.
const LABEL: usize = 60;

pub struct Record {
    directory: PathBuf,
    shots: Option<Shots>,
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
        Ok(Self { directory, shots })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// With `--screenshots`, each display as it is after the check `name`. A screenshot that
    /// fails fails no check: it is said so, and the run goes on.
    pub fn shoot(&mut self, name: &str) {
        let Some(shots) = &mut self.shots else {
            return;
        };
        if shots.displays.is_empty() {
            return;
        }
        shots.taken += 1;
        let label = format!("{:02}-{}", shots.taken, label(name));
        if let Err(error) = view::shot(&shots.tool, &self.directory, &label, &shots.displays) {
            println!("screenshot {label}: {error}");
        }
    }

    /// Every container's log into the record; says where it is. What can't be had is noted in
    /// its file instead.
    pub fn keep_logs(&self, the_box: &TestBox) {
        for container in the_box.containers() {
            let log = the_box
                .log(&container)
                .unwrap_or_else(|error| error.to_string());
            self.write(&format!("{container}.log"), &log);
        }
        println!("record: {}", self.directory.display());
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
