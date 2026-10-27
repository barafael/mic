//! The room a frontend is in, through `mic-testrelay`: a join replaces the room it was in, and a
//! leave leaves it, as the others see.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::{App, Client, Config, Invite, Profile, Trust};
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

/// Waits until `bob` sees `count` others.
async fn sees(bob: &mic_core::Room, count: usize) {
    let mut state = bob.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.len() == count),
    )
    .await
    .unwrap_or_else(|_| panic!("{:?}", bob.state().borrow().participants))
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_replaces_the_room_and_a_leave_leaves_it() {
    let relay = TestRelay::start().await.unwrap();
    let first = Invite::new_room();
    let second = Invite::new_room();
    let bob = client(&relay)
        .join(first.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    let app = App::new(client(&relay));
    let mut rooms = app.room();
    assert!(rooms.borrow_and_update().is_none());

    let joined = app
        .join(first.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    assert!(rooms.has_changed().unwrap());
    assert_eq!(rooms.borrow_and_update().as_ref().unwrap().id(), first.room);
    sees(&bob, 1).await;

    // Into another room: Bob sees Alice go from his.
    app.join(second.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    assert_eq!(app.current().unwrap().id(), second.room);
    assert!(joined.command(mic_core::Command::Leave).await.is_err());
    sees(&bob, 0).await;

    // And back, then out.
    app.join(first, Profile::new("Alice")).await.unwrap();
    sees(&bob, 1).await;
    assert!(app.leave().await);
    assert!(app.current().is_none());
    sees(&bob, 0).await;
    // Leaving no room is no failure.
    assert!(app.leave().await);
}
