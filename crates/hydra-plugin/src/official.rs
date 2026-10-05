//! Signed official packages carried by the application release.

include!(concat!(env!("OUT_DIR"), "/official_plugins.rs"));

pub(crate) const KEY: &str = include_str!("official.pub");

/// Whether this build carries official plugin packages.
pub fn bundled() -> bool {
    !PACKAGES.is_empty()
}

#[cfg(test)]
mod tests {
    #[test]
    fn publisher_pin_matches_release_signing_key() {
        assert_eq!(super::KEY, include_str!("../../../plugins/official.pub"));
    }
}
