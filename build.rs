fn main() {
    println!("cargo:rerun-if-env-changed=STARTUP_LAUNCHER_VERSION");
    let version = std::env::var("STARTUP_LAUNCHER_VERSION").unwrap_or_else(|_| {
        format!(
            "v{}",
            std::env::var("CARGO_PKG_VERSION").expect("package version is set")
        )
    });
    println!("cargo:rustc-env=STARTUP_LAUNCHER_VERSION={version}");
    println!("cargo:rerun-if-changed=assets/app.ico");
    tauri_build::build()
}
