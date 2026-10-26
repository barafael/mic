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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_image_is_pinned_by_digest() {
        let (_, digest) = ARCH.value.split_once("@sha256:").expect("a digest");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
