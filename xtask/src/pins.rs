//! Every upstream the box's images build from, in one place. An image takes the pins it names as
//! build arguments, and its tag hashes them, so a bump here rebuilds exactly the images that use
//! the pin. A bump is its own commit, with the phases rerun.

/// A pin: the build argument's name, as the Containerfiles declare it, and its value.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    pub name: &'static str,
    pub value: &'static str,
}

/// Arch Linux, the base of every image: its Mesa is close to the host's kernel driver, which the
/// GPU in the client pods needs (PLAN.md, "The host's GPU in the box"). Pinned by digest.
pub const ARCH: Pin = Pin {
    name: "ARCH",
    value: "docker.io/library/archlinux:base-20261018.0.612668@sha256:52dfbd5afb23a220d5ee92dafcb4c788d96c026ed04179890c23adef4c897fa0",
};

/// The day of the Arch Linux Archive that pacman installs from, so packages are pinned too:
/// <https://archive.archlinux.org/repos/>`ARCH_SNAPSHOT`.
pub const ARCH_SNAPSHOT: Pin = Pin {
    name: "ARCH_SNAPSHOT",
    value: "2026/10/25",
};

/// moq-dev/moq at the commit of its release `moq-relay-v0.17.8`: the relay, and moq-tokio's
/// `clock` example, built with that commit's Cargo.lock.
pub const MOQ_REV: Pin = Pin {
    name: "MOQ_REV",
    value: "225ab82a401b5731f726cd9bfbece08162bd63f5",
};

/// The Rust toolchain the builder compiles with: rust-toolchain.toml's, which a test holds it
/// to, so the box runs what CI checked.
pub const RUST: Pin = Pin {
    name: "RUST",
    value: "1.99.1",
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The versions of moq's crates at `MOQ_REV`, which mic's own crates use: the box's relay and
    /// `mic-testrelay`'s are then the same code, and mic speaks to the relay it's tested against. A
    /// test holds Cargo.lock to them.
    const MOQ_CRATES: [(&str, &str); 4] = [
        ("moq-relay", "0.17.8"),
        ("moq-tokio", "0.19.29"),
        ("moq-net", "0.3.16"),
        ("moq-e2ee", "0.0.17"),
    ];

    #[test]
    fn the_base_image_is_pinned_by_digest() {
        let (_, digest) = ARCH.value.split_once("@sha256:").expect("a digest");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_snapshot_is_a_day() {
        let parts: Vec<&str> = ARCH_SNAPSHOT.value.split('/').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            [4, 2, 2]
        );
    }

    #[test]
    fn moq_is_pinned_by_commit() {
        assert_eq!(MOQ_REV.value.len(), 40);
        assert!(MOQ_REV.value.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mic_uses_the_moq_crates_of_the_boxs_relay() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
        for (name, version) in MOQ_CRATES {
            let locked: Vec<&str> = lock
                .split("[[package]]")
                .filter(|package| package.contains(&format!("name = \"{name}\"\n")))
                .filter_map(|package| package.split("version = \"").nth(1))
                .filter_map(|rest| rest.split('"').next())
                .collect();
            assert_eq!(locked, [version], "{name} in Cargo.lock");
        }
    }

    #[test]
    fn the_builder_compiles_with_the_repositorys_toolchain() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let toolchain = std::fs::read_to_string(repo.join("rust-toolchain.toml")).unwrap();
        assert!(
            toolchain.contains(&format!("channel = \"{}\"", RUST.value)),
            "rust-toolchain.toml and pins::RUST differ"
        );
    }
}
