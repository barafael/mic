//! A room's state, commands and events, through `mic-testrelay`.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::session::Error;
use mic_core::{Client, Command, Config, Connection, Event, Invite, Profile, Trust};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

/// A client of `relay`; a connection gives up after 2 s, so a room's redials are quick to see.
fn client(relay: &TestRelay) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.connect_timeout = Duration::from_secs(2);
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_state_says_who_and_where_and_commands_change_it() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let state = alice.state().borrow().clone();
    assert_eq!(state.room, invite.room.to_string());
    assert_eq!(state.me.key, alice.participant());
    assert_eq!(state.me.profile.name, "Alice");
    assert_eq!(state.connection, Connection::Connected);
    assert!(state.participants.is_empty());

    alice
        .command(Command::SetName {
            name: " Alice B. ".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(alice.state().borrow().me.profile.name, "Alice B.");

    alice.command(Command::Leave).await.unwrap();
    assert_eq!(alice.state().borrow().connection, Connection::Left);
    assert!(matches!(
        alice.command(Command::Leave).await,
        Err(Error::Left)
    ));
}

/// The room's next event, within [`PATIENCE`].
async fn next(events: &mut mic_core::room::Events) -> Event {
    tokio::time::timeout(PATIENCE, events.next())
        .await
        .expect("an event in time")
        .expect("the room's events go on")
}

#[tokio::test(flavor = "multi_thread")]
async fn arrivals_and_departures_are_events() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let mut events = alice.events();
    let bob = client(&relay)
        .join(invite, Profile::new("Bob"))
        .await
        .unwrap();
    match next(&mut events).await {
        Event::Arrived { participant } => {
            assert_eq!(participant.key, bob.participant());
            assert_eq!(participant.profile.name, "Bob");
        }
        other => panic!("not Bob's arrival: {other:?}"),
    }
    // Dropping every handle leaves the room, as the command does.
    drop(bob);
    match next(&mut events).await {
        Event::Departed { participant } => assert_eq!(participant.profile.name, "Bob"),
        other => panic!("not Bob's departure: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_state_and_commands_are_plain_data() {
    let command = Command::SetName {
        name: "Alice".to_owned(),
    };
    let json = serde_json::to_string(&command).unwrap();
    assert_eq!(json, r#"{"command":"set_name","name":"Alice"}"#);
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);

    let relay = TestRelay::start().await.unwrap();
    let alice = client(&relay)
        .join(Invite::new_room(), Profile::new("Alice"))
        .await
        .unwrap();
    let state = alice.state().borrow().clone();
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(
        serde_json::from_str::<mic_core::RoomState>(&json).unwrap(),
        state
    );
}

/// A frontend's thread runs no tokio (Slint's event loop doesn't): a command awaited there, on
/// another executor, is done all the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_from_a_thread_without_tokio_is_done() {
    let relay = TestRelay::start().await.unwrap();
    let alice = client(&relay)
        .join(Invite::new_room(), Profile::new("Alice"))
        .await
        .unwrap();
    let handle = alice.clone();
    std::thread::spawn(move || {
        pollster::block_on(handle.command(Command::SetName {
            name: "From elsewhere".to_owned(),
        }))
    })
    .join()
    .unwrap()
    .unwrap();
    assert_eq!(alice.state().borrow().me.profile.name, "From elsewhere");
}

/// Waits until `room`'s connection is `connection`.
async fn connection_is(room: &mic_core::Room, connection: Connection) {
    let mut state = room.state();
    let reached = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.connection == connection),
    )
    .await;
    assert!(
        reached.is_ok(),
        "{:?}, not {connection:?}",
        room.state().borrow().connection
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_room_reconnects_after_the_relay_restarts() {
    let mut relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite, Profile::new("Bob"))
        .await
        .unwrap();
    let mut state = alice.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.len() == 1),
    )
    .await
    .unwrap()
    .unwrap();

    relay.stop().await.unwrap();
    connection_is(&alice, Connection::Reconnecting).await;
    connection_is(&bob, Connection::Reconnecting).await;
    // Longer than a connection's redial window (the connect timeout), so the room dials anew.
    tokio::time::sleep(Duration::from_secs(3)).await;
    relay.restart().await.unwrap();
    connection_is(&alice, Connection::Connected).await;
    connection_is(&bob, Connection::Connected).await;
    // Bob, once, as he was.
    let mut state = alice.state();
    let back = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| {
            state.participants.len() == 1 && state.participants[0].key == bob.participant()
        }),
    )
    .await;
    assert!(back.is_ok(), "{:?}", alice.state().borrow().participants);
}
