//! Build script: derive the binary version from the git tag so the release tag
//! is the single source of truth. `Cargo.toml`'s `version` is a frozen `0.0.0`
//! placeholder; the real version is resolved here, in precedence order:
//!
//! 1. `RABSODY_VERSION_OVERRIDE` env var (CI passes the stripped tag, so release
//!    builds need no git history / tag fetch).
//! 2. `git describe --tags --match 'rabsody-v*' --dirty`, with the `rabsody-v`
//!    prefix stripped (e.g. `rabsody-v0.2.0` -> `0.2.0`).
//! 3. `CARGO_PKG_VERSION` (the `0.0.0` placeholder) when git is unavailable.
//!
//! The result is exposed to the crate as `env!("RABSODY_VERSION")`.

use std::path::Path;
use std::process::Command;

fn main() {
    let version = resolve_version();
    println!("cargo:rustc-env=RABSODY_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RABSODY_VERSION_OVERRIDE");
    watch_git_ref_state();
}

/// Re-run the build script when the git ref state (HEAD / tags) changes, so the
/// embedded version refreshes on new commits and tags. `git rev-parse
/// --git-path` resolves each path against the real git dir, so this works
/// regardless of where the crate sits in the tree and in worktrees/submodules
/// (where `.git` is a file pointer and tags live in a separate common dir).
/// Best-effort: silently skip if git is unavailable (e.g. a source tarball), so
/// a missing `.git` never forces a perpetual rebuild.
fn watch_git_ref_state() {
    for spec in ["HEAD", "packed-refs", "refs/tags"] {
        if let Some(path) = git_path(spec)
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

/// Resolve a git ref path (e.g. `HEAD`, `refs/tags`) against the real git dir.
/// Returns `None` if git is unavailable or the lookup fails.
fn git_path(spec: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", spec])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!path.is_empty()).then_some(path)
}

fn resolve_version() -> String {
    if let Ok(override_version) = std::env::var("RABSODY_VERSION_OVERRIDE") {
        let trimmed = override_version.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(described) = git_describe() {
        return described;
    }
    std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
}

/// `git describe` against the `rabsody-v*` tag namespace, with the prefix
/// stripped. Returns `None` on any failure (no git, no matching tag, shallow
/// clone) so the caller falls through to the `CARGO_PKG_VERSION` placeholder
/// rather than failing the build.
fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--match", "rabsody-v*", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let described = String::from_utf8(output.stdout).ok()?;
    let described = described.trim();
    let stripped = described.strip_prefix("rabsody-v").unwrap_or(described);
    (!stripped.is_empty()).then(|| stripped.to_string())
}
