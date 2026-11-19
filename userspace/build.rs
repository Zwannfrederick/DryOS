use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("b0387ab"));
    println!("cargo:rustc-env=DRYOS_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=DRYOS_KERNEL_VERSION=0.8.0-ascension8");
}
