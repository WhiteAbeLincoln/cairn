//! SPA asset resolution: filesystem (`--web-dir`) or compiled-in embed
//! (`web-ui` feature). Unknown paths are the caller's problem — this module
//! only resolves a *known* relative path to bytes + content type; the SPA
//! fallback-to-`index.html` behavior lives in `serve::http`.

use std::path::{Path, PathBuf};

/// The SPA build embedded at compile time when the `web-ui` feature is
/// enabled. Guarded against a missing `cairn-web/build` by `build.rs`, which
/// fails the build early with a clear message instead of letting
/// `include_dir!` panic opaquely during macro expansion.
#[cfg(feature = "web-ui")]
static EMBEDDED: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../cairn-web/build");

/// One served asset: its bytes and MIME type.
pub(crate) struct Asset {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: &'static str,
}

/// A resolved SPA asset source.
pub(crate) enum Assets {
    /// `--web-dir <path>`.
    Dir(PathBuf),
    /// Compile-time embed (`web-ui` feature), used when `--web-dir` is absent.
    #[cfg(feature = "web-ui")]
    Embedded,
}

impl Assets {
    /// Resolve the asset source for `--web-ui`/`--web-dir`.
    ///
    /// Errors if `web_dir` is given but isn't a directory, or if neither
    /// `web_dir` nor the `web-ui` embed feature is available — the two ways
    /// `--web-ui` can have no assets to serve.
    pub(crate) fn resolve(web_dir: Option<&Path>) -> anyhow::Result<Self> {
        if let Some(dir) = web_dir {
            anyhow::ensure!(
                dir.is_dir(),
                "--web-dir {} is not a directory",
                dir.display()
            );
            return Ok(Self::Dir(dir.to_path_buf()));
        }

        #[cfg(feature = "web-ui")]
        {
            Ok(Self::Embedded)
        }
        #[cfg(not(feature = "web-ui"))]
        {
            anyhow::bail!(
                "--web-ui needs SPA assets: pass --web-dir <path>, or build the daemon with \
                 --features web-ui to embed cairn-web/build"
            )
        }
    }

    /// Fetch a path relative to the asset root (no leading slash, already
    /// rejected traversal). Returns `None` if it doesn't exist as a file.
    pub(crate) fn get(&self, rel_path: &str) -> Option<Asset> {
        if !is_safe_rel_path(rel_path) {
            return None;
        }
        match self {
            Self::Dir(dir) => {
                let bytes = std::fs::read(dir.join(rel_path)).ok()?;
                Some(Asset {
                    body: bytes,
                    content_type: mime_for(rel_path),
                })
            }
            #[cfg(feature = "web-ui")]
            Self::Embedded => {
                let file = EMBEDDED.get_file(rel_path)?;
                Some(Asset {
                    body: file.contents().to_vec(),
                    content_type: mime_for(rel_path),
                })
            }
        }
    }

    /// The SPA entry point served for unknown paths (client-side routing).
    pub(crate) fn index(&self) -> Option<Asset> {
        self.get("index.html")
    }
}

/// Reject absolute paths and `..` traversal segments. `PathBuf::join` with an
/// absolute path silently replaces the base instead of erroring, and a `..`
/// segment can walk outside `dir` even when the joined path's *string* still
/// starts with `dir`'s — so both must be checked before ever touching the
/// filesystem.
fn is_safe_rel_path(rel_path: &str) -> bool {
    !rel_path.starts_with('/') && !rel_path.split('/').any(|seg| seg == "..")
}

fn mime_for(path: &str) -> &'static str {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_without_web_dir_or_feature_errors() {
        // This test only proves something when the `web-ui` feature is off
        // (the crate's default `cargo test` configuration); with the feature
        // on, `resolve(None)` legitimately succeeds via the embed.
        #[cfg(not(feature = "web-ui"))]
        {
            assert!(Assets::resolve(None).is_err());
        }
    }

    #[test]
    fn resolve_rejects_nonexistent_web_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(Assets::resolve(Some(&missing)).is_err());
    }

    #[test]
    fn dir_serves_known_file_with_mime_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<html>hi</html>").unwrap();
        std::fs::write(dir.path().join("app.js"), b"console.log(1)").unwrap();

        let assets = Assets::resolve(Some(dir.path())).unwrap();

        let index = assets.get("index.html").unwrap();
        assert_eq!(index.body, b"<html>hi</html>");
        assert_eq!(index.content_type, "text/html");

        let js = assets.get("app.js").unwrap();
        assert_eq!(js.body, b"console.log(1)");
        assert_eq!(js.content_type, "text/javascript");
    }

    #[test]
    fn dir_index_helper_matches_get_index_html() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<html>hi</html>").unwrap();
        let assets = Assets::resolve(Some(dir.path())).unwrap();
        assert_eq!(assets.index().unwrap().body, b"<html>hi</html>");
    }

    #[test]
    fn unknown_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"hi").unwrap();
        let assets = Assets::resolve(Some(dir.path())).unwrap();
        assert!(assets.get("nope.txt").is_none());
    }

    #[test]
    fn traversal_attempts_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"hi").unwrap();
        // A file that genuinely exists just outside `dir`, to prove the
        // rejection isn't just "file not found".
        let parent = dir.path().parent().unwrap();
        std::fs::write(parent.join("secret.txt"), b"do not serve me").unwrap();

        let assets = Assets::resolve(Some(dir.path())).unwrap();
        assert!(assets.get("../secret.txt").is_none());
        assert!(
            assets
                .get(&format!("/{}", parent.join("secret.txt").display()))
                .is_none()
        );
    }

    #[test]
    fn is_safe_rel_path_rejects_absolute_and_traversal() {
        assert!(is_safe_rel_path("index.html"));
        assert!(is_safe_rel_path("assets/app.js"));
        assert!(!is_safe_rel_path("/etc/passwd"));
        assert!(!is_safe_rel_path("../secret"));
        assert!(!is_safe_rel_path("foo/../../secret"));
    }
}
