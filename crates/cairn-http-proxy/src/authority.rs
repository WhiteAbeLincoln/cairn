use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use base64::Engine as _;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use hudsucker::rustls::crypto::aws_lc_rs;

pub struct ProxyAuthority {
    cert_pem: String,
    key_pem: String,
    ca_path: PathBuf,
    bundle_path: PathBuf,
    runtime_dir: PathBuf,
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
        })
    }

    pub fn authority(&self) -> anyhow::Result<RcgenAuthority> {
        let key = KeyPair::from_pem(&self.key_pem).context("parsing proxy CA key")?;
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, key)
            .context("parsing proxy CA certificate")?;
        Ok(RcgenAuthority::new(
            issuer,
            1_000,
            aws_lc_rs::default_provider(),
        ))
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

fn native_bundle(cairn_ca: &str) -> String {
    let mut output = String::new();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        output.push_str("-----BEGIN CERTIFICATE-----\n");
        let encoded = base64::engine::general_purpose::STANDARD.encode(cert.as_ref());
        for line in encoded.as_bytes().chunks(64) {
            output.push_str(std::str::from_utf8(line).unwrap_or_default());
            output.push('\n');
        }
        output.push_str("-----END CERTIFICATE-----\n");
    }
    for key in [
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
        "NODE_EXTRA_CA_CERTS",
    ] {
        if let Some(path) = std::env::var_os(key)
            && let Ok(contents) = std::fs::read_to_string(path)
        {
            output.push_str(&contents);
            if !contents.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output.push_str(cairn_ca);
    output
}
