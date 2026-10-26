//! The harness's server, driven by its client, as the box's checks drive a participant, through
//! `mic-testrelay`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::{Command, Config, Invite, RoomState, Trust};
use mic_harness::client::Client;
use mic_harness::server::Harness;
use mic_harness::{Acted, Call, Done, Joined, Pong, Snapshot, Subscribed};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

/// A harness for a client of `relay`, served at a socket of its own; returns the socket.
fn serve(relay: &TestRelay, name: &str, directory: &Path) -> PathBuf {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    let client = mic_core::Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let socket = directory.join(name);
    let harness = Harness::new(client, name, None);
    let path = socket.clone();
    tokio::spawn(async move { harness.serve(&path).await });
    socket
}

/// A client of the harness at `socket`, once it listens.
fn connect(socket: &Path) -> Client {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        match Client::connect(socket, PATIENCE) {
            Ok(client) => return client,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{}: {error}", socket.display()),
        }
    }
}

/// Reads pushes until one's room satisfies `holds`.
fn pushed_until(client: &mut Client, holds: impl Fn(&RoomState) -> bool) -> Snapshot {
    loop {
        let pushed = client.next_pushed().unwrap();
        if pushed.snapshot.room.as_ref().is_some_and(&holds) {
            return pushed.snapshot;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_drives_two_participants_through_their_harnesses() {
    let relay = TestRelay::start().await.unwrap();
    let directory = std::env::temp_dir().join(format!("mic-harness-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let alice = serve(&relay, "alice", &directory);
    let bob = serve(&relay, "bob", &directory);
    let invite = Invite::new_room().to_mic_url();

    tokio::task::spawn_blocking(move || {
        let mut alice = connect(&alice);
        let mut bob = connect(&bob);
        let pong: Pong = alice.call(Call::Ping).unwrap();
        assert_eq!(pong.name, "alice");
        let snapshot: Snapshot = alice.call(Call::Snapshot).unwrap();
        assert_eq!(snapshot.room, None);
        assert_eq!(snapshot.ui, None);

        let joined: Joined = alice
            .call(Call::Join {
                invite: invite.clone(),
                name: "Alice".to_owned(),
            })
            .unwrap();
        assert_eq!(joined.state.me.profile.name, "Alice");
        let _subscribed: Subscribed = alice.call(Call::Subscribe).unwrap();
        let _joined: Joined = bob
            .call(Call::Join {
                invite,
                name: "Bob".to_owned(),
            })
            .unwrap();
        pushed_until(&mut alice, |room| {
            room.participants
                .iter()
                .any(|them| them.profile.name == "Bob")
        });

        let _acted: Acted = bob
            .call(Call::Act {
                command: Command::SetName {
                    name: "Robert".to_owned(),
                },
            })
            .unwrap();
        pushed_until(&mut alice, |room| {
            room.participants
                .iter()
                .any(|them| them.profile.name == "Robert")
        });

        let done: Done = bob.call(Call::Reset).unwrap();
        assert!(done.ok);
        pushed_until(&mut alice, |room| room.participants.is_empty());
        let in_no_room = bob.call::<Acted>(Call::Act {
            command: Command::Leave,
        });
        assert!(in_no_room.unwrap_err().contains("in no room"));
        let snapshot: Snapshot = bob.call(Call::Snapshot).unwrap();
        assert_eq!(snapshot.room, None);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn what_isnt_a_request_is_answered_with_an_error() {
    let relay = TestRelay::start().await.unwrap();
    let directory = std::env::temp_dir().join(format!("mic-harness-bad-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let socket = serve(&relay, "carol", &directory);
    tokio::task::spawn_blocking(move || {
        let _client = connect(&socket);
        use std::io::{BufRead, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream.write_all(b"{\"method\": \"dance\"}\n").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.starts_with(r#"{"id":0,"error":"not a request"#),
            "{line}"
        );
    })
    .await
    .unwrap();
}
