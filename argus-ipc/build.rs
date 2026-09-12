fn main() {
    println!("cargo:rerun-if-env-changed=ARGUS_BUILD_ID");
    println!("cargo:rerun-if-changed=src/lib.rs");
    let id = std::env::var("ARGUS_BUILD_ID").unwrap_or_else(|_| {
        let git = std::process::Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".into());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!("{git}-{now}")
    });
    println!("cargo:rustc-env=ARGUS_BUILD_ID={id}");
}
