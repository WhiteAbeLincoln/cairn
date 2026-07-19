use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use hudsucker::certificate_authority::{CertificateAuthority, RcgenAuthority};
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use hudsucker::rustls::ServerConfig;
use hudsucker::rustls::crypto::aws_lc_rs;

pub struct ProxyAuthority {
    cert_pem: String,
    key_pem: String,
    ca_path: PathBuf,
    bundle_path: PathBuf,
    runtime_dir: PathBuf,
    /// The built `RcgenAuthority` (and its leaf-cert cache), memoized so every
    /// `ProxySession` sharing this `ProxyAuthority` reuses one CA instead of
    /// re-parsing the PEM and paying for a cold leaf-cert cache each time.
    shared_ca: OnceLock<SharedCa>,
}

/// A cheaply-cloneable handle to a single, shared [`RcgenAuthority`].
///
/// `RcgenAuthority` is not `Clone`, so this newtype wraps it in an `Arc` and
/// forwards [`CertificateAuthority`] to the shared inner instance. Rust's
/// orphan rules forbid `impl CertificateAuthority for Arc<RcgenAuthority>`
/// directly (both the trait and `RcgenAuthority` are foreign), hence the
/// local wrapper type.
#[derive(Clone)]
pub struct SharedCa(Arc<RcgenAuthority>);

impl CertificateAuthority for SharedCa {
    async fn gen_server_config(&self, authority: &http::uri::Authority) -> Arc<ServerConfig> {
        self.0.gen_server_config(authority).await
    }
}

static NEXT_AUTHORITY_ID: AtomicU64 = AtomicU64::new(0);

impl ProxyAuthority {
    pub fn create(parent: &Path) -> anyhow::Result<Self> {
        let authority_id = NEXT_AUTHORITY_ID.fetch_add(1, Ordering::Relaxed);
        let runtime_dir = parent.join(format!("proxy-{}-{authority_id}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(&runtime_dir).with_context(|| {
            format!("creating proxy runtime directory {}", runtime_dir.display())
        })?;

        let key = KeyPair::generate().context("generating proxy CA key")?;
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, "Cairn Session Proxy CA");
        params.distinguished_name = distinguished_name;
        let cert = params.self_signed(&key).context("signing proxy CA")?;
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();

        let ca_path = runtime_dir.join("ca.pem");
        write_private_file(&ca_path, cert_pem.as_bytes())?;
        let bundle_path = runtime_dir.join("ca-bundle.pem");
        let bundle = native_bundle(&cert_pem);
        write_private_file(&bundle_path, bundle.as_bytes())?;

        Ok(Self {
            cert_pem,
            key_pem,
            ca_path,
            bundle_path,
            runtime_dir,
            shared_ca: OnceLock::new(),
        })
    }

    /// Returns a cheaply-cloneable handle to this authority's `RcgenAuthority`.
    ///
    /// The underlying CA (and its leaf-cert cache) is built once and memoized
    /// in `shared_ca`, so every call — and every `ProxySession` created from
    /// this `ProxyAuthority` — shares the same instance instead of re-parsing
    /// the CA PEM and starting from a cold leaf-cert cache each time.
    pub fn authority(&self) -> anyhow::Result<SharedCa> {
        if let Some(ca) = self.shared_ca.get() {
            return Ok(ca.clone());
        }
        let key = KeyPair::from_pem(&self.key_pem).context("parsing proxy CA key")?;
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, key)
            .context("parsing proxy CA certificate")?;
        let ca = SharedCa(Arc::new(RcgenAuthority::new(
            issuer,
            1_000,
            aws_lc_rs::default_provider(),
        )));
        Ok(self.shared_ca.get_or_init(|| ca).clone())
    }

    pub fn ca_path(&self) -> &Path {
        &self.ca_path
    }

    pub fn bundle_path(&self) -> &Path {
        &self.bundle_path
    }
}

impl Drop for ProxyAuthority {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.runtime_dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %self.runtime_dir.display(), "proxy runtime cleanup failed");
        }
    }
}

fn write_private_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("writing {}", path.display()))
}

/// IO-performing wrapper: loads native root certs and env-provided bundles,
/// hands them to the pure [`build_bundle`] assembler, and surfaces any
/// native-cert loading errors via a `tracing::warn!` (loudly, but without
/// failing session creation — an empty/partial native store is plausible in
/// dev environments and the cairn CA is included regardless).
fn native_bundle(cairn_ca: &str) -> String {
    let native = rustls_native_certs::load_native_certs();
    let native_der_certs: Vec<Vec<u8>> = native
        .certs
        .iter()
        .map(|cert| cert.as_ref().to_vec())
        .collect();
    let native_errors: Vec<String> = native.errors.iter().map(ToString::to_string).collect();
    let env_bundles = read_env_ca_bundles();

    let bundle = build_bundle(&native_der_certs, &native_errors, cairn_ca, &env_bundles);

    if !bundle.errors.is_empty() {
        tracing::warn!(
            loaded = bundle.loaded,
            errors = ?bundle.errors,
            "loading native root certificates reported errors; injected CA bundle may be missing system trust roots"
        );
    }

    bundle.pem
}

/// Reads whatever PEM bundles are already pointed to by the CA-bundle env
/// vars a proxied child might inherit, so they get folded into the bundle we
/// inject rather than dropped when we override those vars.
fn read_env_ca_bundles() -> Vec<String> {
    [
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
        "NODE_EXTRA_CA_CERTS",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .collect()
}

/// Outcome of assembling the trust bundle: the final PEM text, how many
/// native root certs were loaded, and any errors reported while loading them.
struct NativeBundle {
    pem: String,
    loaded: usize,
    errors: Vec<String>,
}

/// Pure assembly of the injected CA bundle from already-loaded inputs: native
/// root certs (as DER), any errors reported while loading them, PEM bundles
/// collected from the environment, and finally the cairn proxy's own CA
/// certificate. Kept separate from the IO in [`native_bundle`] so the
/// error-surfacing and PEM-encoding behavior can be tested without touching
/// the real platform certificate store.
fn build_bundle(
    native_der_certs: &[Vec<u8>],
    native_errors: &[String],
    cairn_ca: &str,
    env_bundles: &[String],
) -> NativeBundle {
    let mut pem_out = String::new();
    for der in native_der_certs {
        pem_out.push_str(&pem::encode(&pem::Pem::new("CERTIFICATE", der.clone())));
    }
    for contents in env_bundles {
        pem_out.push_str(contents);
        if !contents.ends_with('\n') {
            pem_out.push('\n');
        }
    }
    pem_out.push_str(cairn_ca);

    NativeBundle {
        pem: pem_out,
        loaded: native_der_certs.len(),
        errors: native_errors.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two calls to `authority()` on the same `ProxyAuthority` must be backed
    /// by the same `RcgenAuthority` instance (and therefore the same
    /// leaf-cert cache), not a freshly parsed/constructed one each time.
    #[test]
    fn authority_reuses_shared_ca_across_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proxy_authority = ProxyAuthority::create(dir.path()).expect("create authority");

        let first = proxy_authority.authority().expect("first authority build");
        let second = proxy_authority.authority().expect("second authority build");

        assert!(
            std::sync::Arc::ptr_eq(&first.0, &second.0),
            "authority() must return a handle to the same shared RcgenAuthority, not rebuild it"
        );
    }

    /// When native root-certificate loading reports errors, the pure bundle
    /// builder must surface them (rather than silently discarding them like
    /// the original implementation), while still including the cairn CA in
    /// the resulting bundle so session TLS trust isn't left empty.
    #[test]
    fn build_bundle_surfaces_native_errors_but_keeps_cairn_ca() {
        let native_der_certs: Vec<Vec<u8>> = Vec::new();
        let native_errors = vec!["permission denied reading system keychain".to_string()];
        let cairn_ca = "-----BEGIN CERTIFICATE-----\nQ0FJUk5DQQ==\n-----END CERTIFICATE-----\n";

        let bundle = build_bundle(&native_der_certs, &native_errors, cairn_ca, &[]);

        assert_eq!(bundle.loaded, 0);
        assert_eq!(bundle.errors, native_errors);
        assert!(bundle.pem.contains(cairn_ca));
    }

    /// The bundle assembled from native DER certs must be valid, parseable
    /// PEM (using the `pem` crate rather than hand-rolled base64 chunking).
    #[test]
    fn build_bundle_produces_valid_pem_blocks() {
        let der = vec![0x30, 0x82, 0x01, 0x02, 0x03, 0x04];
        let cairn_ca = "-----BEGIN CERTIFICATE-----\nQ0FJUk5DQQ==\n-----END CERTIFICATE-----\n";

        let bundle = build_bundle(std::slice::from_ref(&der), &[], cairn_ca, &[]);

        let parsed = pem::parse_many(&bundle.pem).expect("bundle should be valid concatenated PEM");
        assert_eq!(
            parsed.len(),
            2,
            "expected one PEM block for the native cert and one for the cairn CA"
        );
        assert_eq!(parsed[0].contents(), der.as_slice());
        assert!(bundle.errors.is_empty());
    }
}
