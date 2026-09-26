use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use time::OffsetDateTime;

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
            let certificate =
                SelfSignedCertificate::generate(hostname).map_err(io::Error::other)?;
            fs::create_dir_all(directory)?;
            fs::write(&files.key, certificate.key_pem)?;
            fs::write(&files.certificate, certificate.certificate_pem)?;
            tracing::info!(
                "created a self-signed certificate in {}; browsers will ask you to accept it",
                directory.display()
            );
        }
        Ok(files)
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
        let files = CertificateFiles::ensure_self_signed(&tls_directory, "agent-box")
            .expect("certificate is created");
        let first_certificate = read(&files.certificate);
        assert!(first_certificate.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(read(&files.key).contains("PRIVATE KEY"));

        CertificateFiles::ensure_self_signed(&tls_directory, "agent-box")
            .expect("certificate is reused");
        assert_eq!(read(&files.certificate), first_certificate);
    }

    #[test]
    fn old_certificate_is_replaced() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let files = CertificateFiles::ensure_self_signed(directory.path(), "agent-box")
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

        CertificateFiles::ensure_self_signed(directory.path(), "agent-box")
            .expect("certificate is renewed");
        assert_ne!(read(&files.certificate), first_certificate);
    }
}
