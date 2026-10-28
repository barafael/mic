//! `mic-admit`: the auth server for a `moq-relay` that mic's rooms use.
//!
//! ```sh
//! mic-admit --listen 127.0.0.1:8081
//! moq-relay --auth-url http://127.0.0.1:8081 …
//! ```
//!
//! The relay accepts plain `http://` for a loopback server only; beyond that, `https://`, or a
//! unix socket.

use std::net::SocketAddr;
use std::sync::Arc;

use mic_admit::{Admission, Policy};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut listen: SocketAddr = ([127, 0, 0, 1], 8081).into();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--listen", Some(addr)) => {
                listen = addr
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("`{addr}` isn't an address")));
            }
            _ => usage("unknown argument"),
        }
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("mic-admit listening on {}", listener.local_addr()?);
    mic_admit::serve(listener, Arc::new(Admission::new(Policy::default()))).await
}

fn usage(why: &str) -> ! {
    eprintln!("{why}\nusage: mic-admit [--listen ADDR:PORT]");
    std::process::exit(2)
}
