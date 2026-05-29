//! TLS certificate handling for the WebTransport listener.
//!
//! Generates self-signed ECDSA P-256 certificates, loads PEM files, and exports
//! SPKI hashes for client-side certificate pinning.
//!
//! ECDSA P-256 is required by the W3C WebTransport spec for `serverCertificateHashes`
//! pinning — Ed25519 is rejected by compliant clients.

use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use ring::digest;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject as _;

/// TLS configuration holding PEM-encoded cert/key strings and the SPKI hash
/// of the certificate (for WebTransport client-side pinning).
pub struct TlsConfig {
    pub cert_pem: String,
    pub key_pem: String,
    /// SHA-256 hash of the certificate DER bytes.
    pub spki_hash: Vec<u8>,
}

impl TlsConfig {
    /// Load a TLS configuration from existing PEM files on disk.
    pub fn from_pem_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let cert_pem = std::fs::read_to_string(cert_path)
            .with_context(|| format!("reading cert PEM from {}", cert_path.display()))?;
        let key_pem = std::fs::read_to_string(key_path)
            .with_context(|| format!("reading key PEM from {}", key_path.display()))?;
        let spki_hash =
            compute_spki_hash(&cert_pem).context("computing SPKI hash from loaded cert PEM")?;
        Ok(Self {
            cert_pem,
            key_pem,
            spki_hash,
        })
    }

    /// Generate or reuse a self-signed ECDSA P-256 certificate in `tls_dir`.
    ///
    /// If `tls_dir/cert.pem` and `tls_dir/key.pem` already exist and the cert
    /// has not expired (checked by file modification time — files modified more
    /// than 13 days ago are considered expired), the existing files are reused.
    /// Otherwise a new 14-day cert is generated and written to disk.
    pub fn self_signed(tls_dir: &Path) -> Result<Self> {
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        if cert_path.exists() && key_path.exists() && !is_expired(&cert_path) {
            return Self::from_pem_files(&cert_path, &key_path);
        }

        let (cert_pem, key_pem) =
            generate_self_signed().context("generating self-signed TLS certificate")?;

        std::fs::create_dir_all(tls_dir)
            .with_context(|| format!("creating TLS directory {}", tls_dir.display()))?;

        write_pem_file(&cert_path, &cert_pem, 0o644).context("writing cert.pem")?;
        write_pem_file(&key_path, &key_pem, 0o600).context("writing key.pem")?;

        let spki_hash =
            compute_spki_hash(&cert_pem).context("computing SPKI hash from generated cert")?;

        Ok(Self {
            cert_pem,
            key_pem,
            spki_hash,
        })
    }

    /// Write the hex-encoded SPKI hash to `path`.
    pub fn export_hash(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.spki_hash_hex())
            .with_context(|| format!("writing SPKI hash to {}", path.display()))
    }

    /// Return the hex-encoded SPKI hash as a `String`.
    pub fn spki_hash_hex(&self) -> String {
        hex_encode(&self.spki_hash)
    }
}

/// Generate a self-signed ECDSA P-256 certificate valid for 14 days, with
/// "localhost" as a Subject Alternative Name.
///
/// ECDSA P-256 is required for `serverCertificateHashes` pinning per the
/// W3C WebTransport spec — Ed25519 is not accepted by compliant clients.
///
/// Returns `(cert_pem, key_pem)`.
pub fn generate_self_signed() -> Result<(String, String)> {
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .context("generating ECDSA P-256 key pair")?;

    let mut params = CertificateParams::new(vec!["localhost".to_string()])
        .context("building certificate params")?;

    // Set a 14-day validity window from now.
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(14);

    let cert = params
        .self_signed(&key_pair)
        .context("self-signing certificate")?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    Ok((cert_pem, key_pem))
}

/// Compute the SHA-256 hash of the certificate DER bytes from a PEM string.
pub fn compute_spki_hash(cert_pem: &str) -> Result<Vec<u8>> {
    let cert_der = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .context("parsing certificate PEM to DER")?;

    let hash = digest::digest(&digest::SHA256, cert_der.as_ref());
    Ok(hash.as_ref().to_vec())
}

/// Hex-encode a byte slice.
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Returns `true` if the file's modification time is older than 13 days,
/// which means a 14-day cert written alongside it is expired (or nearly so).
fn is_expired(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        // If we can't get the mtime, treat as expired so we regenerate.
        return true;
    };
    let Ok(elapsed) = modified.elapsed() else {
        // System clock went backwards — treat as not expired.
        return false;
    };
    elapsed > std::time::Duration::from_secs(13 * 24 * 60 * 60)
}

/// Write PEM content to a file, creating parent directories as needed, and set
/// Unix file permissions.
fn write_pem_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    std::fs::write(path, content)
        .with_context(|| format!("writing PEM file {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    // On non-Unix platforms the mode argument is ignored.
    #[cfg(not(unix))]
    let _ = mode;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_generates_valid_pem() {
        let (cert, key) = generate_self_signed().unwrap();
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(key.starts_with("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn spki_hash_is_32_bytes() {
        let (cert, _) = generate_self_signed().unwrap();
        let hash = compute_spki_hash(&cert).unwrap();
        assert_eq!(hash.len(), 32, "SHA-256 should produce 32 bytes");
    }

    #[test]
    fn self_signed_reuses_unexpired_cert() {
        let dir = tempfile::tempdir().unwrap();
        let c1 = TlsConfig::self_signed(dir.path()).unwrap();
        let c2 = TlsConfig::self_signed(dir.path()).unwrap();
        assert_eq!(c1.spki_hash, c2.spki_hash, "should reuse the same cert");
    }

    #[test]
    fn export_and_read_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TlsConfig::self_signed(dir.path()).unwrap();
        let hash_path = dir.path().join("cert-hash");
        cfg.export_hash(&hash_path).unwrap();
        let read_back = std::fs::read_to_string(&hash_path).unwrap();
        assert_eq!(read_back, cfg.spki_hash_hex());
    }

    #[test]
    fn pem_loading_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = generate_self_signed().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert).unwrap();
        std::fs::write(&key_path, &key).unwrap();
        let cfg = TlsConfig::from_pem_files(&cert_path, &key_path).unwrap();
        assert_eq!(cfg.cert_pem, cert);
        assert_eq!(cfg.spki_hash.len(), 32);
    }
}
