// libghostty-vt-sys 0.2.0 statically links ghostty's C/C++ code but its
// build.rs doesn't emit link directives for transitive static dependencies
// (highway, simdutf, utfcpp) or the C++ stdlib. Work around it here until
// upstream fixes it.
fn main() {
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=stdc++");
    }

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let target_dir = std::path::Path::new(&out_dir)
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .unwrap()
        .to_path_buf();

    if let Some(sys_out) = find_sys_out_dir(&target_dir) {
        let zig_cache = sys_out.join("zig-cache").join("o");
        if zig_cache.exists() {
            for lib in &["highway", "simdutf", "utfcpp"] {
                if let Some(dir) = find_lib_in_cache(&zig_cache, lib) {
                    println!("cargo:rustc-link-search=native={}", dir.display());
                    println!("cargo:rustc-link-lib=static={lib}");
                }
            }
        }
    }
}

fn find_lib_in_cache(zig_cache: &std::path::Path, lib_name: &str) -> Option<std::path::PathBuf> {
    let target = format!("lib{lib_name}.a");
    for entry in std::fs::read_dir(zig_cache).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir() {
            let dir = entry.path();
            if dir.join(&target).exists() {
                return Some(dir);
            }
        }
    }
    None
}

fn find_sys_out_dir(target_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(target_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("libghostty-vt-sys-") {
            let out = entry.path().join("out");
            if out.join("zig-cache").exists() {
                return Some(out);
            }
        }
    }
    None
}
