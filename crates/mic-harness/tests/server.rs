//! The harness's server, driven by its client, as the box's checks drive a participant, through
//! `mic-testrelay`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::{App, Command, Config, Invite, Profile, RoomState, Trust};
use mic_harness::client::Client;
use mic_harness::server::Harness;
use mic_harness::{Acted, Call, Done, Joined, Pong, Snapshot, Subscribed};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

/// A harness for a client of `relay`, served at a socket of its own; returns the socket, and the
/// app the harness drives, as a frontend's screens would.
fn serve(relay: &TestRelay, name: &str, directory: &Path) -> (PathBuf, App) {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    let client = mic_core::Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let socket = directory.join(name);
    let app = App::new(client);
    let harness = Harness::new(app.clone(), name, None);
    let path = socket.clone();
    tokio::spawn(async move { harness.serve(&path).await });
    (socket, app)
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
    let (alice, _) = serve(&relay, "alice", &directory);
    let (bob, _) = serve(&relay, "bob", &directory);
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
    let (socket, _) = serve(&relay, "carol", &directory);
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

#[tokio::test(flavor = "multi_thread")]
async fn a_room_the_frontend_joins_is_the_harnesss_too() {
    let relay = TestRelay::start().await.unwrap();
    let directory = std::env::temp_dir().join(format!("mic-harness-app-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (socket, app) = serve(&relay, "dave", &directory);
    let invite = Invite::new_room();
    let mut harness = tokio::task::spawn_blocking(move || {
        let mut harness = connect(&socket);
        let _subscribed: Subscribed = harness.call(Call::Subscribe).unwrap();
        harness
    })
    .await
    .unwrap();
    // As the join screen would.
    app.join(invite.clone(), Profile::new("Dave"))
        .await
        .unwrap();
    let app_left = app.clone();
    tokio::task::spawn_blocking(move || {
        let snapshot = pushed_until(&mut harness, |room| room.me.profile.name == "Dave");
        assert_eq!(snapshot.room.unwrap().room, invite.room.to_string());
        // As a click on "leave" would, from another thread: the harness sees it go.
        let runtime = tokio::runtime::Handle::current();
        assert!(runtime.block_on(app_left.leave()));
        loop {
            if harness.next_pushed().unwrap().snapshot.room.is_none() {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_is_the_owners_only_and_replaces_a_former_one() {
    use std::os::unix::fs::PermissionsExt;
    let relay = TestRelay::start().await.unwrap();
    let directory = std::env::temp_dir().join(format!("mic-harness-mode-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    // A socket file left from a former run.
    let former = std::os::unix::net::UnixListener::bind(directory.join("erin"));
    drop(former);
    let (socket, _) = serve(&relay, "erin", &directory);
    tokio::task::spawn_blocking(move || {
        let mut client = connect(&socket);
        let _pong: Pong = client.call(Call::Ping).unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let left: Vec<_> = std::fs::read_dir(socket.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".binding"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    })
    .await
    .unwrap();
}
