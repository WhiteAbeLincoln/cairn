//! Guards the `web-ui` feature's compile-time SPA embed.
//!
//! `serve::assets` embeds `cairn-web/build` via `include_dir!` when the
//! `web-ui` feature is enabled. Left unguarded, a missing build directory
//! turns into an opaque proc-macro panic; check for it here instead so the
//! failure is a clear, actionable `cargo::error=` message before we ever get
//! to macro expansion.
fn main() {
    // CARGO_MANIFEST_DIR is set by cargo for every build script invocation;
    // its absence would mean cargo itself is broken, not a condition this
    // build can recover from.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let web_build = std::path::Path::new(&manifest_dir)
        .join("..")
        .join("..")
        .join("cairn-web")
        .join("build");

    // Re-run if the build output appears/disappears/changes, so toggling
    // `npm run build` is picked up without a `cargo clean`.
    println!("cargo::rerun-if-changed={}", web_build.display());

    if std::env::var_os("CARGO_FEATURE_WEB_UI").is_none() {
        // Feature disabled: cairn-web/build is never touched, so a plain
        // `cargo build` has no npm-artifact dependency.
        return;
    }

    if !web_build.join("index.html").exists() {
        println!(
            "cargo::error=the `web-ui` feature embeds {} at compile time, but index.html is \
             missing there; build the SPA first (`npm run build` in cairn-web/), or drop \
             `--features web-ui` and pass `--web-dir <path>` to the daemon at runtime instead",
            web_build.display()
        );
        std::process::exit(1);
    }
}
