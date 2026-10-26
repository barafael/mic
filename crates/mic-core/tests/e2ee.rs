//! Every track is encrypted under the room's secret (D17), through `mic-testrelay`.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::tracks::{PeerEvent, RoomCrypto};
use mic_core::{Client, Config, Invite, Room, Trust};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);
/// What a participant writes; nothing on the relay may carry it in the clear.
const PLAINTEXT: &str = "alice's very secret note";

fn client(relay: &TestRelay) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

/// `room` publishes a track `note` with one group holding [`PLAINTEXT`]; returns its producer,
/// which keeps the track up.
fn publish_note(room: &mut Room) -> moq_e2ee::track::Producer {
    let mut track = room.publish("note").unwrap();
    let mut group = track.append_group().unwrap();
    group
        .write_frame(moq_net::Timestamp::now(), PLAINTEXT.as_bytes())
        .unwrap();
    group.finish().unwrap();
    track
}

/// The first peer to arrive in `room`.
async fn first_peer(room: &Room) -> mic_core::tracks::Peer {
    let mut peers = room.peers();
    loop {
        match tokio::time::timeout(PATIENCE, peers.next()).await {
            Ok(Some(Ok(PeerEvent::Arrived(peer)))) => return peer,
            Ok(Some(Ok(PeerEvent::Gone(_)))) => continue,
            Ok(Some(Err(error))) => panic!("a peer: {error}"),
            Ok(None) => panic!("the peers ended"),
            Err(_) => panic!("no peer within {PATIENCE:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_participant_reads_anothers_track_decrypted() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let mut alice = client(&relay).join(invite.clone()).await.unwrap();
    let _note = publish_note(&mut alice);
    let bob = client(&relay).join(invite).await.unwrap();

    let peer = first_peer(&bob).await;
    assert_eq!(peer.path(), alice.path());
    let mut track = peer.track("note").await.unwrap();
    let mut group = tokio::time::timeout(PATIENCE, track.recv_group())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame = group.read_frame().await.unwrap().unwrap();
    assert_eq!(frame.plaintext, PLAINTEXT.as_bytes());
}

#[tokio::test(flavor = "multi_thread")]
async fn another_room_sees_nothing_of_it() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let mut alice = client(&relay).join(invite.clone()).await.unwrap();
    let _note = publish_note(&mut alice);
    // Another room, and the same room's id with another secret: neither finds alice.
    let elsewhere = client(&relay).join(Invite::new_room()).await.unwrap();
    let forged = Invite {
        room: invite.room,
        secret: mic_core::RoomSecret::random(),
    };
    let guessing = client(&relay).join(forged).await.unwrap();
    for room in [&elsewhere, &guessing] {
        let mut peers = room.peers();
        let found = tokio::time::timeout(Duration::from_secs(1), peers.next()).await;
        assert!(found.is_err(), "a room without the secret found a peer");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_relay_sees_only_opaque_names_and_ciphertext() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let mut alice = client(&relay).join(invite.clone()).await.unwrap();
    let _note = publish_note(&mut alice);

    // A snoop on the relay: a plain moq client subscribing to every announcement.
    let origin = moq_tokio::origin::spawn();
    let snoop = relay.connect_config().init(Default::default()).unwrap();
    let _session = snoop.with_subscriber(origin.clone()).connect(relay.url());
    let consumer = origin.consume();
    let mut announced = consumer.announced();
    let path = loop {
        let update = tokio::time::timeout(PATIENCE, announced.next())
            .await
            .unwrap()
            .unwrap();
        if update.kind.is_active() {
            break update.prefix.as_str().to_owned();
        }
    };
    let crypto = RoomCrypto::new(&invite).unwrap();
    assert_eq!(
        path,
        format!("{}/{}", crypto.prefix().as_str(), alice.path())
    );
    let room_id = invite.room.to_string();
    let participant: String = alice
        .key()
        .public()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(!path.contains(&room_id) && !path.contains(&participant));

    // The track's name is opaque too; the relay learns it from a subscriber, as this does.
    let generation = crypto.generation_of(alice.path()).unwrap();
    let name = generation.name("note").unwrap();
    assert_ne!(name.as_str(), "note");
    let broadcast = consumer.request_broadcast(path.as_str()).await.unwrap();
    let raw = broadcast
        .track(name.as_str())
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut raw = raw;
    let mut group = tokio::time::timeout(PATIENCE, raw.recv_group())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame = group.read_frame().await.unwrap().unwrap();
    let ciphertext = frame.payload.to_vec();
    assert!(!ciphertext.is_empty());
    assert!(
        !ciphertext
            .windows(PLAINTEXT.len())
            .any(|window| window == PLAINTEXT.as_bytes()),
        "the relay carries the plaintext"
    );

    // And a key of another secret doesn't open it: an error, not garbage.
    let wrong = RoomCrypto::new(&Invite {
        room: invite.room,
        secret: mic_core::RoomSecret::random(),
    })
    .unwrap();
    let raw = broadcast
        .track(name.as_str())
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut wrongly = wrong
        .generation_of(alice.path())
        .unwrap()
        .consume(raw)
        .unwrap();
    let opened = match tokio::time::timeout(PATIENCE, wrongly.recv_group())
        .await
        .unwrap()
    {
        Ok(Some(mut group)) => group.read_frame().await,
        Ok(None) => panic!("the track ended"),
        Err(error) => Err(error),
    };
    assert!(
        matches!(opened, Err(moq_e2ee::Error::Authentication)),
        "opened with another secret: {opened:?}"
    );
}
