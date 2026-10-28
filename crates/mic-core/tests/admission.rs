//! A relay that lets only members in, and each only where it publishes (§5.11, P6): a
//! `moq-relay` asking `mic-admit`, as one of a person's own runs with `--auth-url`. Through
//! `mic-testrelay`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use mic_admit::{Admission, PARAM, Policy, Proof};
use mic_core::secrets::InMemory;
use mic_core::tracks::{PeerEvent, RoomCrypto};
use mic_core::{Client, Config, DeviceSecret, Invite, Profile, Room, Trust};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

/// `mic-admit` on a loopback port, and a relay asking it.
async fn relay() -> TestRelay {
    relay_with(Policy::default()).await
}

async fn relay_with(policy: Policy) -> TestRelay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    tokio::spawn(mic_admit::serve(listener, Arc::new(Admission::new(policy))));
    TestRelay::start_asking(url).await.unwrap()
}

fn client_with(relay: &TestRelay, admission: bool, timeout: Duration) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.admission = admission;
    config.connect_timeout = timeout;
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

fn client(relay: &TestRelay) -> Client {
    client_with(relay, true, PATIENCE)
}

async fn first_peer(room: &Room) -> mic_core::tracks::Peer {
    let mut peers = room.peers();
    loop {
        match tokio::time::timeout(PATIENCE, peers.next()).await {
            Ok(Some(Ok(PeerEvent::Arrived(peer)))) => return peer,
            Ok(Some(Ok(PeerEvent::Gone(_)))) => continue,
            Ok(Some(Err(error))) => panic!("a peer: {error}"),
            _ => panic!("no peer arrived"),
        }
    }
}

/// A plain moq session into the room `invite` names, at `relay`'s URL with `proof` (if any) in
/// its query, publishing `origin`'s broadcasts and subscribing to the room's.
fn session(
    relay: &TestRelay,
    invite: &Invite,
    proof: Option<&Proof>,
    origin: &moq_net::origin::Producer,
) -> moq_tokio::Connection {
    let crypto = RoomCrypto::new(invite).unwrap();
    let mut url = relay.url();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(crypto.prefix().as_str());
    if let Some(proof) = proof {
        url.query_pairs_mut().append_pair(PARAM, &proof.encode());
    }
    relay
        .connect_config()
        .init(Default::default())
        .unwrap()
        .with_origin(origin.clone())
        .connect(url)
}

/// What `device` would present in the room of `invite`.
fn proof_of(invite: &Invite, device: &DeviceSecret) -> Proof {
    let crypto = RoomCrypto::new(invite).unwrap();
    mic_core::admission::proof(invite, &crypto, &device.room_key(&invite.room)).unwrap()
}

/// What a member of the room `invite` names hears announced for `listening`: paths relative to
/// the room's prefix.
async fn heard(relay: &TestRelay, invite: &Invite, listening: Duration) -> BTreeSet<String> {
    let watching = moq_tokio::origin::spawn();
    let member = DeviceSecret::random();
    let _session = session(relay, invite, Some(&proof_of(invite, &member)), &watching);
    let mut announced = watching.consume().announced();
    let mut heard = BTreeSet::new();
    let deadline = tokio::time::Instant::now() + listening;
    while let Ok(Some(update)) = tokio::time::timeout_at(deadline, announced.next()).await {
        if update.kind.is_active() {
            heard.insert(update.prefix.as_str().to_owned());
        }
    }
    heard
}

/// A broadcast announced at `<segment>/<a new epoch>`: its path, and what keeps it up.
fn announce(
    origin: &moq_net::origin::Producer,
    segment: &str,
) -> (String, moq_net::broadcast::Producer) {
    let path = format!("{segment}/{}", moq_e2ee::Epoch::mint());
    let broadcast = origin.create_broadcast(path.as_str()).unwrap();
    broadcast.announce(Default::default()).unwrap();
    (path, broadcast)
}

#[tokio::test(flavor = "multi_thread")]
async fn members_join_and_see_each_other() {
    let relay = relay().await;
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    assert_eq!(first_peer(&alice).await.path(), bob.path());
    assert_eq!(first_peer(&bob).await.path(), alice.path());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_without_the_proof_is_not_let_in() {
    let relay = relay().await;
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    // The relay lets the connection come up and refuses it after: what the join says isn't the
    // question, but whether the room hears of her.
    let _mallory = client_with(&relay, false, Duration::from_secs(2))
        .join(invite.clone(), Profile::new("Mallory"))
        .await;
    let heard = heard(&relay, &invite, Duration::from_secs(3)).await;
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(heard.iter().any(|path| path == alice.path()), "{heard:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_with_a_proof_of_her_own_is_not_let_into_a_room() {
    let relay = relay().await;
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    // She knows the room's prefix, but not its secret: her proof is valid, of a room key of her
    // own.
    let crypto = RoomCrypto::new(&invite).unwrap();
    let proof = Proof::sign(
        crypto.prefix().as_str(),
        "strangers_path",
        &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        &ed25519_dalek::SigningKey::from_bytes(&[8; 32]),
    )
    .unwrap();
    let origin = moq_tokio::origin::spawn();
    let (_, _kept) = announce(&origin, "strangers_path");
    let _stranger = session(&relay, &invite, Some(&proof), &origin);
    let heard = heard(&relay, &invite, Duration::from_secs(3)).await;
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(heard.iter().any(|path| path == alice.path()), "{heard:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_publishes_at_her_own_path_and_nowhere_else() {
    let relay = relay().await;
    let invite = Invite::new_room();
    let crypto = RoomCrypto::new(&invite).unwrap();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();

    // Mallory holds the invite, so her proof stands; she announces at her own path, and at
    // Alice's.
    let mallory = DeviceSecret::random();
    let mallorys = crypto
        .participant_segment(&mallory.room_key(&invite.room).public().to_bytes())
        .unwrap();
    let alices = alice.path().split('/').next().unwrap().to_owned();
    let origin = moq_tokio::origin::spawn();
    let (own, _own) = announce(&origin, &mallorys);
    let (posing, _posing) = announce(&origin, &alices);
    let _mallory = session(&relay, &invite, Some(&proof_of(&invite, &mallory)), &origin);

    let heard = heard(&relay, &invite, Duration::from_secs(4)).await;
    assert!(
        heard.contains(alice.path()),
        "Alice isn't heard of: {heard:?}"
    );
    assert!(
        heard.contains(&own),
        "Mallory isn't, at her path: {heard:?}"
    );
    assert!(
        !heard.contains(&posing),
        "Mallory was announced at Alice's path: {heard:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_outlasts_its_grant() {
    // A grant lasts twice the cadence the relay asks again at: here two seconds, and the room
    // is still there after five.
    let relay = relay_with(Policy::default().with_revalidate(Duration::from_secs(1))).await;
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let heard = heard(&relay, &invite, Duration::from_secs(2)).await;
    assert!(heard.contains(alice.path()), "Alice is gone: {heard:?}");
}
