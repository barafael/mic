//! Who a relay lets into a room (§5.11, P6).
//!
//! A relay that lets anyone publish anywhere has two problems: strangers spend its egress, and a
//! member can announce another member's path. This crate is the answer for a relay of one's own:
//! a client presents a [`Proof`] in its URL's query, and an auth server, which `moq-relay` asks on
//! every `connect` and `revalidate` (`--auth-url`), grants what the proof earns, no more:
//! subscribing to the room, and publishing under the participant's own path.
//!
//! The server never holds a room's secret, so it can't check a proof against it. What it can
//! check is held by first come: the first proof for a room fixes the room's admission key, and
//! the first for a path fixes its participant's. A later proof must be signed by the same keys.
//! So an outsider, who can't sign as the room, is refused once the room exists, and a member
//! can't take another's path once that one has joined. What this doesn't stop is racing a
//! participant to its path before it joins, which needs the room's secret and the participant's
//! key: a denial of service on one path, and whoever it blocks isn't believed anyway, since a
//! catalog must derive the path it stands at (`mic-core`'s `catalog`).

mod proof;
pub use proof::{PARAM, Proof, ProofError};

#[cfg(feature = "server")]
mod policy;
#[cfg(feature = "server")]
pub use policy::{Admission, Policy, Refusal};

#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
pub use server::{router, serve};
