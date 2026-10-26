//! The box's probe: built by the box's builder, run in its client image (`cargo testbox test
//! box`). It says it runs, and what it is, so a check can tell that the binary it hears is the
//! one just built.

/// What the probe prints first; the `box` phase waits for it.
pub const RUNS: &str = "probe: runs";

fn main() {
    println!("{RUNS}, version {}", env!("CARGO_PKG_VERSION"));
}
