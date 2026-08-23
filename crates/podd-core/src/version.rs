//! Build identity, stamped at compile time by `build.rs`.
//!
//! `CARGO_PKG_VERSION` is the workspace version (`0.0.1`) and is never bumped by
//! the `v*` release tags, so it can't tell two builds apart. These constants
//! carry the real thing — see `build.rs` for how they are derived and what the
//! fallbacks are.

/// Human-readable build version: `git describe --tags --always --dirty` with any
/// leading `v` stripped (`0.2.0`, `0.2.0-4-gdeadbee`, `deadbee-dirty`), the
/// `PODD_VERSION` override, or `<pkg-version>-unknown` when neither is available.
pub const VERSION: &str = env!("PODD_BUILD_VERSION");

/// Short git commit this build came from, or `"unknown"`.
pub const GIT_REV: &str = env!("PODD_BUILD_REV");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_stamped_and_display_ready() {
        assert!(!VERSION.is_empty());
        // The UI renders this as `v{VERSION}`; a second `v` would read as `vv0.2.0`.
        assert!(!VERSION.starts_with('v'), "VERSION must not carry a `v` prefix");
        assert!(!VERSION.contains(char::is_whitespace));
    }

    #[test]
    fn rev_is_stamped() {
        assert!(!GIT_REV.is_empty());
        assert!(!GIT_REV.contains(char::is_whitespace));
    }

    /// The whole point of the stamp: a plain workspace-version build is what we
    /// are trying to stop reporting. In a git checkout (dev + CI) `git describe`
    /// must have won; the bare-`CARGO_PKG_VERSION` string is only acceptable as
    /// part of the honest `-unknown` fallback.
    #[test]
    fn version_is_not_the_stale_package_version() {
        if std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.git")
            .exists()
        {
            assert_ne!(VERSION, env!("CARGO_PKG_VERSION"));
        }
    }
}
