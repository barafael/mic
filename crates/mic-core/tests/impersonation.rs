//! A member of the room posing as another (D18): Mallory holds the room's secret, so she can
//! encrypt under any name and publish at any path in the room, but she can't sign as Alice. Through
//! `mic-testrelay`.

use std::time::Duration;

use mic_core::catalog::{CATALOG, Catalog};
use mic_core::roster::PROFILE;
use mic_core::secrets::InMemory;
use mic_core::tracks::RoomCrypto;
use mic_core::{Client, Config, DeviceSecret, Invite, ParticipantKey, Profile, Room, Trust};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

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

/// What keeps a forged broadcast up.
type Forged = (
    moq_tokio::Connection,
    moq_net::broadcast::Producer,
    Vec<moq_e2ee::track::Producer>,
);

/// A broadcast Mallory publishes in the room `invite` names, with the room's secret: at
/// `<segment>/<a new epoch>`, with `catalog` (bytes, as she made them) and a profile named
/// "Alice".
fn forge(
    relay: &TestRelay,
    invite: &Invite,
    segment: &str,
    catalog: impl Fn(&str) -> Vec<u8>,
) -> Forged {
    let crypto = RoomCrypto::new(invite).unwrap();
    let epoch = moq_e2ee::Epoch::mint();
    let path = format!("{segment}/{epoch}");
    let generation = crypto.generation_of(&path).unwrap();
    let origin = moq_tokio::origin::spawn();
    let broadcast = origin.create_broadcast(path.as_str()).unwrap();
    let mut tracks = Vec::new();
    for (semantic, bytes) in [
        (CATALOG, catalog(epoch.as_str())),
        (PROFILE, br#"{"name": "Alice"}"#.to_vec()),
    ] {
        let name = generation.name(semantic).unwrap();
        let track = broadcast.create_track(name.as_str(), None).unwrap();
        let mut track = generation.produce(track).unwrap();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::now(), &bytes)
            .unwrap();
        group.finish().unwrap();
        tracks.push(track);
    }
    broadcast.announce(Default::default()).unwrap();
    let mut url = relay.url();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(crypto.prefix().as_str());
    let session = relay.connect_config().init(Default::default()).unwrap();
    let connection = session.with_origin(origin).connect(url);
    (connection, broadcast, tracks)
}

/// Waits until `room` shows `rejected` broadcasts that didn't stand.
async fn rejected(room: &Room, rejected: usize) -> Vec<mic_core::roster::Rejected> {
    let mut state = room.state();
    let state = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.rejected.len() == rejected),
    )
    .await
    .unwrap_or_else(|_| panic!("rejected: {:?}", room.state().borrow().rejected))
    .unwrap()
    .clone();
    state.rejected
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_posing_as_another_is_rejected() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    let crypto = RoomCrypto::new(&invite).unwrap();
    let mallory = DeviceSecret::random().room_key(&invite.room);
    let mallorys = ParticipantKey::from_bytes(mallory.public().to_bytes());
    let catalog = |participant: ParticipantKey, epoch: &str| Catalog {
        room: invite.room,
        participant,
        kid: crypto.kid(),
        epoch: epoch.to_owned(),
        seq: 1,
        tracks: vec![CATALOG.to_owned(), PROFILE.to_owned()],
        decoders: Vec::new(),
        renditions: Vec::new(),
        app: "0.1.0".to_owned(),
    };

    // At Alice's path, a catalog Mallory signs as herself: it stands for no one there.
    let alices_segment = crypto
        .participant_segment(alice.participant().as_bytes())
        .unwrap();
    let _at_alices = forge(&relay, &invite, &alices_segment, |epoch| {
        catalog(mallorys, epoch).sign(&mallory)
    });
    // At her own path, a catalog naming Alice: her signature doesn't check as Alice's.
    let mallorys_segment = crypto.participant_segment(mallorys.as_bytes()).unwrap();
    let _naming_alice = forge(&relay, &invite, &mallorys_segment, |epoch| {
        let mut bytes = catalog(mallorys, epoch).sign(&mallory);
        // The participant's key, after the domain, the format and the room: Alice's instead.
        let at = b"mic/v1/catalog\0".len() + 4 + 16;
        bytes[at..at + 32].copy_from_slice(alice.participant().as_bytes());
        bytes
    });

    let mut reasons: Vec<String> = rejected(&bob, 2)
        .await
        .into_iter()
        .map(|rejected| rejected.reason)
        .collect();
    reasons.sort();
    assert!(
        reasons[0].contains("naming a participant whose path this isn't"),
        "{reasons:?}"
    );
    assert!(
        reasons[1].contains("signature doesn't check"),
        "{reasons:?}"
    );
    // Bob sees Alice, once, and nothing of Mallory's.
    let mut state = bob.state();
    let alices = alice.participant();
    let shown = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.iter().any(|them| them.key == alices)),
    )
    .await;
    assert!(shown.is_ok(), "Bob doesn't see Alice");
    let participants = bob.state().borrow().participants.clone();
    assert_eq!(participants.len(), 1, "{participants:?}");
    assert_eq!(participants[0].key, alice.participant());
}
