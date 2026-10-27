//! A relay to run the window against by hand: `cargo run -p mic-testrelay --example serve`, then
//! `mic --relay <url> --fingerprint <fingerprint>` with what it prints.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let relay = mic_testrelay::TestRelay::start().await?;
    println!(
        "--relay {} --fingerprint {}",
        relay.url(),
        relay.fingerprint()
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
