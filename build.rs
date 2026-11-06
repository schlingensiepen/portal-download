//! Build stamp: the artifact says which source revision it was built from.
//! The revision comes from `PORTAL_DOWNLOAD_REVISION` (set by a build pipeline)
//! or, failing that, from `git describe` in the source tree.
use std::process::Command;

fn main() {
    let rev = std::env::var("PORTAL_DOWNLOAD_REVISION").ok().filter(|s| !s.is_empty()).or_else(|| {
        Command::new("git")
            .args(["describe", "--always", "--dirty", "--abbrev=12"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    println!("cargo:rustc-env=PORTAL_DOWNLOAD_BUILD_REVISION={}", rev.unwrap_or_else(|| "unknown".into()));
    println!("cargo:rerun-if-env-changed=PORTAL_DOWNLOAD_REVISION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
