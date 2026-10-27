//! mic's media, toolkit-agnostic: where audio comes from and goes to, real devices and synthetic
//! ones alike, behind one pair of traits ([`audio::Source`], [`audio::Sink`]); and video, its
//! pictures, codecs and synthetic camera ([`video`]) (PLAN.md, "The core and its frontends").

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

pub mod audio;
pub mod video;
