//! Captures the compiler version for the `User-Agent` header (`rust/1.95.0`).

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=DATABRICKS_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTC");
}
