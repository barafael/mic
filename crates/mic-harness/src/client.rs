//! A harness's client, blocking and std only, as `cargo testbox` drives one: a request, then its
//! reply; once subscribed, pushed snapshots in between.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::{Call, Pushed, Reply, Request};

/// A connection to a harness.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Pushes that came while a reply was awaited, oldest first.
    pushed: VecDeque<Pushed>,
    /// What came of a line before a read timed out: the rest comes on the next read.
    partial: Vec<u8>,
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
            pushed: VecDeque::new(),
            partial: Vec::new(),
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
                self.pushed.push_back(pushed);
                continue;
            }
            let reply: Reply<T> = serde_json::from_str(&line)
                .map_err(|error| format!("a reply that isn't one: {error}: {line}"))?;
            if reply.id != id {
                return Err(format!("the reply to {} instead of {id}: {line}", reply.id));
            }
            return reply.outcome.into();
        }
    }

    /// The next pushed snapshot, once subscribed.
    pub fn next_pushed(&mut self) -> Result<Pushed, String> {
        if let Some(pushed) = self.pushed.pop_front() {
            return Ok(pushed);
        }
        let line = self.line()?;
        serde_json::from_str(&line).map_err(|error| format!("not a push: {error}: {line}"))
    }

    /// The next whole line. A read that times out keeps what it got of the line, which the next
    /// call goes on from (`read_until` leaves read bytes in the buffer on an error).
    fn line(&mut self) -> Result<String, String> {
        match self.reader.read_until(b'\n', &mut self.partial) {
            Ok(_) if self.partial.ends_with(b"\n") => {
                let line = std::mem::take(&mut self.partial);
                String::from_utf8(line).map_err(|error| format!("a line that isn't UTF-8: {error}"))
            }
            Ok(_) => Err("the harness closed the connection".to_owned()),
            Err(error) => Err(format!("reading from the harness: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_line_cut_by_a_timeout_is_finished_by_the_next_read() {
        let directory =
            std::env::temp_dir().join(format!("mic-harness-partial-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("socket");
        let _removed = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let mut client = Client::connect(&socket, Duration::from_millis(100)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.write_all(br#"{"seq": 1, "snapshot": "#).unwrap();
        assert!(client.next_pushed().is_err(), "half a line isn't one");
        server
            .write_all(b"{\"room\": null, \"ui\": null}}\n")
            .unwrap();
        assert_eq!(client.next_pushed().unwrap().seq, 1);
    }
}
