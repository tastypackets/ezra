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

pub struct CertificateFiles {
    pub certificate: PathBuf,
    pub key: PathBuf,
}

/// Creates a self-signed certificate when there is none or it is close to expiring.
pub fn ensure_certificate(directory: &Path, hostname: &str) -> io::Result<CertificateFiles> {
    let files = CertificateFiles {
        certificate: directory.join(CERTIFICATE_FILE),
        key: directory.join(KEY_FILE),
    };
    if needs_new_certificate(&files) {
        let (certificate_pem, key_pem) =
            self_signed_certificate(hostname).map_err(io::Error::other)?;
        fs::create_dir_all(directory)?;
        fs::write(&files.key, key_pem)?;
        fs::write(&files.certificate, certificate_pem)?;
        tracing::info!(
            "created a self-signed certificate in {}; browsers will ask you to accept it",
            directory.display()
        );
    }
    Ok(files)
}

fn needs_new_certificate(files: &CertificateFiles) -> bool {
    let certificate_age = fs::metadata(&files.certificate)
        .and_then(|metadata| metadata.modified())
        .map(|modified| modified.elapsed().unwrap_or_default());
    match certificate_age {
        Ok(age) => age > RENEW_AFTER || !files.key.exists(),
        Err(_) => true,
    }
}

fn self_signed_certificate(hostname: &str) -> Result<(String, String), rcgen::Error> {
    let mut parameters = CertificateParams::new(vec!["localhost".to_owned(), hostname.to_owned()])?;
    parameters
        .distinguished_name
        .push(DnType::CommonName, hostname);
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - time::Duration::days(1);
    parameters.not_after = now + time::Duration::days(VALIDITY_DAYS);
    parameters.is_ca = IsCa::ExplicitNoCa;
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key_pair = KeyPair::generate()?;
    let certificate = parameters.self_signed(&key_pair)?;
    Ok((certificate.pem(), key_pair.serialize_pem()))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::SystemTime;

    use super::*;

    #[test]
    fn certificate_is_created_once_and_reused() {
        let directory = tempfile::tempdir().unwrap();
        let tls_directory = directory.path().join("tls");
        let files = ensure_certificate(&tls_directory, "agent-box").unwrap();
        let first_certificate = fs::read_to_string(&files.certificate).unwrap();
        assert!(first_certificate.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(
            fs::read_to_string(&files.key)
                .unwrap()
                .contains("PRIVATE KEY")
        );

        ensure_certificate(&tls_directory, "agent-box").unwrap();
        assert_eq!(
            fs::read_to_string(&files.certificate).unwrap(),
            first_certificate
        );
    }

    #[test]
    fn old_certificate_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let files = ensure_certificate(directory.path(), "agent-box").unwrap();
        let first_certificate = fs::read_to_string(&files.certificate).unwrap();
        let long_ago = SystemTime::now() - RENEW_AFTER - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&files.certificate)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        ensure_certificate(directory.path(), "agent-box").unwrap();
        assert_ne!(
            fs::read_to_string(&files.certificate).unwrap(),
            first_certificate
        );
    }
}
