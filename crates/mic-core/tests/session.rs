//! A client's sessions with a relay: `mic-testrelay`, in the test process.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::session::Error;
use mic_core::{Client, Config, Invite, Trust};
use mic_testrelay::TestRelay;

/// A client of `relay`, trusting its certificate by fingerprint.
fn client(relay: &TestRelay, timeout: Duration) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.connect_timeout = timeout;
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_joins_a_room_and_leaves_it() {
    let relay = TestRelay::start().await.unwrap();
    let client = client(&relay, Duration::from_secs(10));
    let invite = Invite::new_room();
    let room = client.join(invite.clone()).await.unwrap();
    assert_eq!(room.invite(), &invite);
    room.leave().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_room_keeps_the_devices_key_across_joins() {
    let relay = TestRelay::start().await.unwrap();
    let client = client(&relay, Duration::from_secs(10));
    let invite = Invite::new_room();
    let first = client.join(invite.clone()).await.unwrap();
    let key = first.key().public();
    first.leave().await.unwrap();
    let again = client.join(invite).await.unwrap();
    assert_eq!(again.key().public(), key);
    again.leave().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_that_isnt_there_is_said_so_in_time() {
    let mut relay = TestRelay::start().await.unwrap();
    let client = client(&relay, Duration::from_secs(2));
    relay.stop().await.unwrap();
    let started = std::time::Instant::now();
    match client.join(Invite::new_room()).await {
        Err(Error::Unreachable { after, .. }) => assert_eq!(after, Duration::from_secs(2)),
        Err(other) => panic!("not unreachable: {other}"),
        Ok(_) => panic!("joined a relay that isn't there"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_with_another_certificate_is_refused() {
    let relay = TestRelay::start().await.unwrap();
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec!["00".repeat(32)]);
    config.connect_timeout = Duration::from_secs(2);
    let client = Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    match client.join(Invite::new_room()).await {
        Err(Error::Unreachable { why, .. }) => {
            assert!(
                why.contains("fingerprint"),
                "the reason names the fingerprint: {why}"
            );
        }
        Err(other) => panic!("not refused for its certificate: {other}"),
        Ok(_) => panic!("joined a relay with another certificate"),
    }
}
