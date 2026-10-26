//! What a phase leaves behind for looking into it once the box is gone:
//! `target/testbox-record/<phase>/`, with every container's log at the phase's end.

use std::path::{Path, PathBuf};

use crate::error::{self, Result};
use crate::podman::{RECORD, TestBox};

pub struct Record {
    directory: PathBuf,
}

impl Record {
    /// The record in `target/testbox-record/<name>`, emptied of a former run's.
    pub fn new(repo: &Path, name: &str) -> Result<Self> {
        let directory = repo.join(RECORD).join(name);
        if directory.exists() {
            std::fs::remove_dir_all(&directory).map_err(|source| error::Error::Io {
                what: directory.display().to_string(),
                source,
            })?;
        }
        error::create_dir_all(&directory)?;
        Ok(Self { directory })
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
