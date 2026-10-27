//! Checkpoint signatures (D18, step 2) through `mic-testrelay`: honest rooms raise no alarm,
//! however long they run and whenever someone joins, and frames no checkpoint covers reject the
//! broadcast within the grace.

use std::time::{Duration, Instant};

use mic_core::catalog::{CATALOG, Catalog};
use mic_core::roster::PROFILE;
use mic_core::secrets::InMemory;
use mic_core::sig::{GRACE, SIG};
use mic_core::tracks::RoomCrypto;
use mic_core::{Client, Command, Config, DeviceSecret, Invite, ParticipantKey, Profile, Trust};
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

#[tokio::test(flavor = "multi_thread")]
async fn an_honest_room_raises_no_alarm() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    // Bob comes late: what Alice wrote at the start is covered for him all the same.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let bob = client(&relay)
        .join(invite, Profile::new("Bob"))
        .await
        .unwrap();
    let mut state = bob.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.len() == 1),
    )
    .await
    .unwrap()
    .unwrap();
    alice
        .command(Command::SetName {
            name: "Alice B.".to_owned(),
        })
        .await
        .unwrap();
    // Longer than the grace, with a change in it.
    tokio::time::sleep(GRACE + Duration::from_secs(2)).await;
    for room in [&alice, &bob] {
        let state = room.state().borrow().clone();
        assert!(state.rejected.is_empty(), "{:?}", state.rejected);
        assert_eq!(state.participants.len(), 1, "{:?}", state.participants);
    }
    assert_eq!(
        bob.state().borrow().participants[0].profile.name,
        "Alice B."
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_no_checkpoint_covers_reject_the_broadcast() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let bob = client(&relay)
        .join(invite.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    // Mallory: a catalog that stands, signed by her own key at her own path, and a profile, but
    // no checkpoints, as anyone writing frames they can't sign would publish.
    let crypto = RoomCrypto::new(&invite).unwrap();
    let mallory = DeviceSecret::random().room_key(&invite.room);
    let participant = ParticipantKey::from_bytes(mallory.public().to_bytes());
    let epoch = moq_e2ee::Epoch::mint();
    let path = format!(
        "{}/{epoch}",
        crypto.participant_segment(participant.as_bytes()).unwrap()
    );
    let generation = crypto.generation_of(&path).unwrap();
    let catalog = Catalog {
        room: invite.room,
        participant,
        kid: crypto.kid(),
        epoch: epoch.as_str().to_owned(),
        seq: 0,
        tracks: vec![CATALOG.to_owned(), SIG.to_owned(), PROFILE.to_owned()],
        app: "0.1.0".to_owned(),
    };
    let origin = moq_tokio::origin::spawn();
    let broadcast = origin.create_broadcast(path.as_str()).unwrap();
    let mut tracks = Vec::new();
    for (semantic, frame) in [
        (CATALOG, Some(catalog.sign(&mallory))),
        (SIG, None),
        (PROFILE, Some(br#"{"name": "Mallory"}"#.to_vec())),
    ] {
        let name = generation.name(semantic).unwrap();
        let track = broadcast.create_track(name.as_str(), None).unwrap();
        let mut track = generation.produce(track).unwrap();
        if let Some(frame) = frame {
            let mut group = track.append_group().unwrap();
            group
                .write_frame(moq_net::Timestamp::now(), &frame)
                .unwrap();
            group.finish().unwrap();
        }
        tracks.push(track);
    }
    broadcast.announce(Default::default()).unwrap();
    let mut url = relay.url();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(crypto.prefix().as_str());
    let _session = relay
        .connect_config()
        .init(Default::default())
        .unwrap()
        .with_origin(origin)
        .connect(url);

    let started = Instant::now();
    let mut state = bob.state();
    let rejected =
        tokio::time::timeout(PATIENCE, state.wait_for(|state| !state.rejected.is_empty()))
            .await
            .expect("rejected in time")
            .unwrap()
            .rejected
            .clone();
    assert!(
        rejected[0].reason.contains("no signed checkpoint covered"),
        "{rejected:?}"
    );
    // Within the grace, and the time a check takes after it.
    assert!(
        started.elapsed() < GRACE + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(bob.state().borrow().participants.is_empty());
}
