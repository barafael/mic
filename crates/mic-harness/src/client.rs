//! A harness's client, blocking and std only, as `cargo testbox` drives one: a request, then its
//! reply; once subscribed, pushed snapshots in between.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::{Call, Outcome, Pushed, Reply, Request};

/// A connection to a harness.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Pushes that came while a reply was awaited, oldest first.
    pushed: Vec<Pushed>,
}

impl Client {
    /// Connects to the harness at `socket`; a read waits at most `patience` for a line.
    pub fn connect(socket: &Path, patience: Duration) -> std::io::Result<Self> {
        let writer = UnixStream::connect(socket)?;
        writer.set_read_timeout(Some(patience))?;
        Ok(Self {
            reader: BufReader::new(writer.try_clone()?),
            writer,
            next_id: 1,
            pushed: Vec::new(),
        })
    }

    /// Sends `call`, and returns its reply's result as `T`, or the harness's error.
    pub fn call<T: DeserializeOwned>(&mut self, call: Call) -> Result<T, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_string(&Request { id, call }).map_err(|error| error.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|error| format!("writing to the harness: {error}"))?;
        loop {
            let line = self.line()?;
            if let Ok(pushed) = serde_json::from_str::<Pushed>(&line) {
                self.pushed.push(pushed);
                continue;
            }
            let reply: Reply<T> = serde_json::from_str(&line)
                .map_err(|error| format!("a reply that isn't one: {error}: {line}"))?;
            if reply.id != id {
                return Err(format!("the reply to {} instead of {id}: {line}", reply.id));
            }
            return match reply.outcome {
                Outcome::Ok(result) => Ok(result),
                Outcome::Err(error) => Err(error),
            };
        }
    }

    /// The next pushed snapshot, once subscribed.
    pub fn next_pushed(&mut self) -> Result<Pushed, String> {
        if !self.pushed.is_empty() {
            return Ok(self.pushed.remove(0));
        }
        let line = self.line()?;
        serde_json::from_str(&line).map_err(|error| format!("not a push: {error}: {line}"))
    }

    fn line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err("the harness closed the connection".to_owned()),
            Ok(_) => Ok(line),
            Err(error) => Err(format!("reading from the harness: {error}")),
        }
    }
}
