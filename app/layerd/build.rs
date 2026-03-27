// build.rs
use std::process::Command;
fn main() {
    // TODO: this doesn't pick up the hash properly in the docker build
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let git_hash = String::from_utf8(output.stdout).unwrap();
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    let output = Command::new("cargo").args(["-V"]).output().unwrap();
    let output_str = String::from_utf8(output.stdout).unwrap();
    let rust_version = output_str.split(' ').nth(1).unwrap();
    println!("cargo:rustc-env=RUST_VERSION={}", rust_version);
}
