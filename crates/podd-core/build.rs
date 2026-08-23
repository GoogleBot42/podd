//! Build-time version stamping for podd.
//!
//! `CARGO_PKG_VERSION` is the workspace version (`0.0.1`) and is never bumped by
//! the `v*` release tags, so it is useless for telling two builds apart during a
//! deploy. This script stamps the real build identity into the binary instead:
//!
//! * `PODD_BUILD_VERSION` — `git describe --tags --always --dirty` (leading `v`
//!   stripped), e.g. `0.2.0`, `0.2.0-4-gdeadbee`, `deadbee-dirty`.
//! * `PODD_BUILD_REV` — the short commit hash.
//!
//! Nix flake sources do not carry `.git`, so both values can be supplied by the
//! environment instead (`PODD_VERSION` / `PODD_GIT_REV`, set from `self.shortRev`
//! in `flake.nix`). With neither git nor env available the version degrades to
//! `<pkg-version>-unknown` — honest about not knowing, never a made-up branch.
//!
//! Nothing here embeds a timestamp: identical inputs produce identical output.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Explicit rerun list. Emitting any `rerun-if-changed` disables cargo's
    // default "rescan the whole package dir", so the crate's own sources have to
    // be listed too.
    for path in ["build.rs", "Cargo.toml", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=PODD_VERSION");
    println!("cargo:rerun-if-env-changed=PODD_GIT_REV");
    emit_git_rerun_paths();

    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    let version = from_env("PODD_VERSION")
        .or_else(|| git(&["describe", "--tags", "--always", "--dirty=-dirty"]))
        .unwrap_or_else(|| format!("{pkg_version}-unknown"));
    // The UI renders this as `v{version}`; don't hand it a second `v`.
    let version = version.strip_prefix('v').unwrap_or(&version);

    let rev = from_env("PODD_GIT_REV")
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=PODD_BUILD_VERSION={version}");
    println!("cargo:rustc-env=PODD_BUILD_REV={rev}");
}

fn from_env(key: &str) -> Option<String> {
    let value = env::var(key).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Run `git` inside the crate's source tree; `None` if git is missing, this is
/// not a repository, or the command produced nothing.
fn git(args: &[&str]) -> Option<String> {
    let dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").current_dir(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Track the git files that decide the stamp, so a commit or checkout re-runs
/// this script instead of leaving a stale version baked into an incremental
/// build. No-op when there is no repository (Nix builds).
fn emit_git_rerun_paths() {
    let mut paths = vec![git_path("HEAD"), git_path("packed-refs")];
    // Committing on a branch rewrites the ref file, not HEAD.
    if let Some(head_ref) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
        paths.push(git_path(&head_ref));
    }
    for path in paths.into_iter().flatten() {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

/// Resolve a path inside the git dir (`git rev-parse --git-path`), which handles
/// worktrees and separate git dirs. Relative results are anchored at the repo root.
fn git_path(name: &str) -> Option<PathBuf> {
    let raw = git(&["rev-parse", "--git-path", name])?;
    let path = Path::new(&raw);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    Some(Path::new(&git(&["rev-parse", "--show-toplevel"])?).join(path))
}
