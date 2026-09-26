use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum_server::tls_rustls::RustlsConfig;
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use x509_parser::pem::parse_x509_pem;

const CERTIFICATE_FILE: &str = "certificate.pem";
const KEY_FILE: &str = "key.pem";
// Safari rejects longer than 825 days.
const VALIDITY_DAYS: i64 = 820;
const RENEW_AFTER: Duration = Duration::from_secs(790 * 24 * 60 * 60);

/// The PEM files the manager serves HTTPS with.
pub struct CertificateFiles {
    pub certificate: PathBuf,
    pub key: PathBuf,
}

impl CertificateFiles {
    fn in_directory(directory: &Path) -> Self {
        Self {
            certificate: directory.join(CERTIFICATE_FILE),
            key: directory.join(KEY_FILE),
        }
    }

    /// Creates a self-signed certificate when there is none or it is close to expiring.
    pub fn ensure_self_signed(directory: &Path, hostname: &str) -> io::Result<Self> {
        let files = Self::in_directory(directory);
        if files.needs_renewal() {
            files.write_self_signed(hostname)?;
        }
        Ok(files)
    }

    fn write_self_signed(&self, hostname: &str) -> io::Result<()> {
        let certificate = SelfSignedCertificate::generate(hostname).map_err(io::Error::other)?;
        if let Some(directory) = self.certificate.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::write(&self.key, certificate.key_pem)?;
        fs::write(&self.certificate, certificate.certificate_pem)?;
        tracing::info!(
            "created a self-signed certificate in {}, so browsers show a warning until you accept it",
            self.certificate.display()
        );
        Ok(())
    }

    fn status(&self) -> io::Result<CertificateStatus> {
        let pem = fs::read(&self.certificate)?;
        let (_, pem) = parse_x509_pem(&pem).map_err(io::Error::other)?;
        let certificate = pem.parse_x509().map_err(io::Error::other)?;
        Ok(CertificateStatus {
            expires_at: certificate.validity().not_after.timestamp(),
        })
    }

    fn needs_renewal(&self) -> bool {
        let certificate_age = fs::metadata(&self.certificate)
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified.elapsed().unwrap_or_default());
        match certificate_age {
            Ok(age) => age > RENEW_AFTER || !self.key.exists(),
            Err(_) => true,
        }
    }
}

/// The certificate the manager serves HTTPS with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CertificateStatus {
    /// When it expires, in seconds since the Unix epoch.
    pub expires_at: i64,
}

/// The certificate on disk and the TLS configuration serving it.
#[derive(Clone)]
pub struct ServedCertificate {
    files: Arc<CertificateFiles>,
    hostname: String,
    pub config: RustlsConfig,
}

impl ServedCertificate {
    /// Serves the certificate in `directory`, creating or renewing it first when needed.
    pub async fn load(directory: &Path, hostname: &str) -> io::Result<Self> {
        let files = CertificateFiles::ensure_self_signed(directory, hostname)?;
        let config = RustlsConfig::from_pem_file(&files.certificate, &files.key).await?;
        Ok(Self {
            files: Arc::new(files),
            hostname: hostname.to_owned(),
            config,
        })
    }

    pub fn status(&self) -> io::Result<CertificateStatus> {
        self.files.status()
    }

    /// Replaces the certificate with a new self-signed one, served from the next connection on.
    pub async fn regenerate(&self) -> io::Result<()> {
        self.files.write_self_signed(&self.hostname)?;
        self.config
            .reload_from_pem_file(&self.files.certificate, &self.files.key)
            .await
    }
}

struct SelfSignedCertificate {
    certificate_pem: String,
    key_pem: String,
}

impl SelfSignedCertificate {
    fn generate(hostname: &str) -> Result<Self, rcgen::Error> {
        let mut parameters =
            CertificateParams::new(vec!["localhost".to_owned(), hostname.to_owned()])?;
        parameters
            .distinguished_name
            .push(DnType::CommonName, hostname);
        let now = OffsetDateTime::now_utc();
        parameters.not_before = now.saturating_sub(time::Duration::days(1));
        parameters.not_after = now.saturating_add(time::Duration::days(VALIDITY_DAYS));
        parameters.is_ca = IsCa::ExplicitNoCa;
        parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key_pair = KeyPair::generate()?;
        let certificate = parameters.self_signed(&key_pair)?;
        Ok(Self {
            certificate_pem: certificate.pem(),
            key_pem: key_pair.serialize_pem(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::SystemTime;

    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("certificate file is readable")
    }

    #[test]
    fn certificate_is_created_once_and_reused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let tls_directory = directory.path().join("tls");
        let files = CertificateFiles::ensure_self_signed(&tls_directory, "ezra")
            .expect("certificate is created");
        let first_certificate = read(&files.certificate);
        assert!(first_certificate.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(read(&files.key).contains("PRIVATE KEY"));

        CertificateFiles::ensure_self_signed(&tls_directory, "ezra")
            .expect("certificate is reused");
        assert_eq!(read(&files.certificate), first_certificate);
    }

    #[test]
    fn status_reads_the_certificate_on_disk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let files = CertificateFiles::ensure_self_signed(directory.path(), "ezra")
            .expect("certificate is created");
        let status = files.status().expect("status is read");
        let in_819_days = OffsetDateTime::now_utc()
            .saturating_add(time::Duration::days(VALIDITY_DAYS.saturating_sub(1)))
            .unix_timestamp();
        assert!(status.expires_at > in_819_days, "{status:?}");
    }

    #[tokio::test]
    async fn regenerating_serves_a_new_certificate() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir().expect("temporary directory");
        let served = ServedCertificate::load(directory.path(), "ezra")
            .await
            .expect("certificate is served");
        let before = fs::read_to_string(&served.files.certificate).expect("certificate is read");
        served
            .regenerate()
            .await
            .expect("certificate is regenerated");
        assert_ne!(
            fs::read_to_string(&served.files.certificate).expect("certificate is read"),
            before
        );
    }

    #[test]
    fn old_certificate_is_replaced() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let files = CertificateFiles::ensure_self_signed(directory.path(), "ezra")
            .expect("certificate is created");
        let first_certificate = read(&files.certificate);
        let long_ago = SystemTime::now()
            .checked_sub(RENEW_AFTER.saturating_add(Duration::from_secs(60)))
            .expect("the clock is past the renewal age");
        File::options()
            .write(true)
            .open(&files.certificate)
            .and_then(|file| file.set_modified(long_ago))
            .expect("certificate modification time can be changed");

        CertificateFiles::ensure_self_signed(directory.path(), "ezra")
            .expect("certificate is renewed");
        assert_ne!(read(&files.certificate), first_certificate);
    }
}
