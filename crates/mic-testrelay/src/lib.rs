//! A MoQ relay inside the test process, for mic's `cargo test`: `moq-relay` embedded, at the
//! version the box runs (xtask/src/pins.rs), on an ephemeral loopback port. So joins, the roster,
//! reconnects and media go through a real relay on every platform CI runs on, which the box
//! (Linux only) doesn't.
//!
//! Its TLS identity is made once and kept for the relay's life, so a [`TestRelay::restart`]
//! comes back on the same port with the same certificate, as a real relay restarts: clients that
//! pinned its fingerprint reconnect to it.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::net::SocketAddr;

use moq_tokio::tls::Identity;

pub use moq_tokio;

/// A running relay, stopped when dropped.
pub struct TestRelay {
    identity: Identity,
    quic: SocketAddr,
    /// The auth server it asks about each session; none, and anyone may publish and subscribe.
    auth: Option<url::Url>,
    running: Option<Running>,
}

/// A relay serving, and the way to stop it.
struct Running {
    trigger: moq_relay::shutdown::Trigger,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl TestRelay {
    /// A relay on an ephemeral loopback port, anyone allowed to publish and subscribe, serving
    /// once this returns.
    pub async fn start() -> anyhow::Result<Self> {
        Self::start_with(None).await
    }

    /// A relay that asks the auth server at `auth` (`http://` on loopback) what each session
    /// may do, as a self-hosted one run with `--auth-url` does.
    pub async fn start_asking(auth: url::Url) -> anyhow::Result<Self> {
        Self::start_with(Some(auth)).await
    }

    async fn start_with(auth: Option<url::Url>) -> anyhow::Result<Self> {
        let identity = Identity::generate(["localhost"])?;
        let (quic, running) = serve(&identity, "127.0.0.1:0".parse()?, auth.as_ref()).await?;
        Ok(Self {
            identity,
            quic,
            auth,
            running: Some(running),
        })
    }

    /// Where clients dial it: its address, not `localhost`, which resolves to `::1` first, where
    /// nothing listens, and an attempt there waits out its timeout.
    pub fn url(&self) -> url::Url {
        format!("https://{}/", self.quic)
            .parse()
            .expect("a URL of a port is valid")
    }

    /// Its certificate's SHA-256 fingerprint, which clients pin (`tls.fingerprint`).
    pub fn fingerprint(&self) -> &str {
        self.identity.fingerprint()
    }

    /// A client's configuration for it: its URL, its fingerprint pinned.
    pub fn connect_config(&self) -> moq_tokio::connect::Config {
        let mut config = moq_tokio::connect::Config::default();
        config.url = Some(self.url());
        config.tls.fingerprint = vec![self.fingerprint().to_owned()];
        config
    }

    /// Stops it: every session ends, as when a relay goes away.
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(running) = self.running.take() {
            running.trigger.start();
            running.task.await??;
        }
        Ok(())
    }

    /// Stops it, then starts it again on the same port with the same certificate.
    pub async fn restart(&mut self) -> anyhow::Result<()> {
        self.stop().await?;
        let (_, running) = serve(&self.identity, self.quic, self.auth.as_ref()).await?;
        self.running = Some(running);
        Ok(())
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.trigger.start();
        }
    }
}

/// A relay with `identity` on `bind`, serving: where it bound, and the way to stop it.
async fn serve(
    identity: &Identity,
    bind: SocketAddr,
    auth: Option<&url::Url>,
) -> anyhow::Result<(SocketAddr, Running)> {
    let mut config = moq_relay::Config::default();
    config.drain_timeout = std::time::Duration::ZERO;
    config.listen.bind = Some(bind.to_string().parse()?);
    config.listen.tls.identity = Some(identity.clone());
    match auth {
        Some(url) => config.auth.url = Some(url.clone()),
        None => config.auth.public = vec![moq_auth::Pattern::all()],
    }
    let relay = moq_relay::Relay::load(config).await?;
    let quic = relay.quic_addr()?;
    let ready = relay.ready();
    let trigger = relay.shutdown_trigger().clone();
    let task = tokio::spawn(relay.run());
    ready.wait().await?;
    Ok((quic, Running { trigger, task }))
}
