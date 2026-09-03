//! Which build a binary is, beside the version it reports. A fleet runs
//! several builds of one version between bumps, and the version alone cannot
//! tell them apart.
//!
//! `METSUKE_REV` is what the flake passes, because a build in the sandbox has
//! no repository to read. A cargo build reads git instead, and a build with
//! neither says so rather than guessing.

fn main() {
    println!("cargo::rerun-if-env-changed=METSUKE_REV");
    let rev = match std::env::var("METSUKE_REV") {
        Ok(passed) if !passed.trim().is_empty() => passed.trim().to_string(),
        _ => from_git().unwrap_or_else(|| "unknown".to_string()),
    };
    println!("cargo::rustc-env=BUILD_REV={rev}");
}

/// The short commit, marked where the tree it was built from carried edits.
fn from_git() -> Option<String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|run| run.status.success())
            .map(|run| String::from_utf8_lossy(&run.stdout).trim().to_string())
    };
    let git_dir = git(&["rev-parse", "--absolute-git-dir"])?;
    // So a commit moving is what rebuilds this, rather than every invocation.
    println!("cargo::rerun-if-changed={git_dir}/HEAD");
    let rev = git(&["rev-parse", "--short=7", "HEAD"])?;
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    Some(match dirty {
        true => format!("{rev}-dirty"),
        false => rev,
    })
}
