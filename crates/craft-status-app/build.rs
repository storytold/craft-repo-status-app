use std::path::Path;
use std::process::Command;

fn main() {
    build_ui();
    tauri_build::build()
}

/// Compiles the TypeScript UI (ui/src/) to ui/build/ before Tauri embeds
/// ui/, so every cargo build of the app ships the current UI.
fn build_ui() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        "ui/src",
        "tsconfig.json",
        "tsconfig.build.json",
        "package.json",
        "package-lock.json",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    if !root.join("node_modules/.bin/tsc").exists() {
        panic!(
            "TypeScript isn't installed: run `npm ci` in {}",
            root.display()
        );
    }
    // On Windows npm is a batch shim, which Command only finds by full name.
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = Command::new(npm)
        .args(["run", "--silent", "build"])
        .current_dir(&root)
        .status()
        .expect("couldn't run npm; the UI build needs Node.js 24+");
    assert!(
        status.success(),
        "the UI's TypeScript failed to compile (npm run build)"
    );
}
