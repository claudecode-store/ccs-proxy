#[cfg(any(target_os = "linux", target_os = "windows", test))]
mod platform;

use anyhow::{Context, Result, ensure};
use rcgen::PublicKeyData;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use time::OffsetDateTime;
use tracing::info;

const RENEW_BEFORE: Duration = Duration::from_secs(30 * 86400);

pub struct Certificates {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub fn default_directory() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .context("找不到 LOCALAPPDATA，请使用 --tls-cert 和 --tls-key 指定证书")?;
        return Ok(PathBuf::from(local).join("ccs-proxy/tls"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(path).join("ccs-proxy/tls"));
        }
        let home = std::env::var_os("HOME")
            .context("找不到用户目录，请使用 --tls-cert 和 --tls-key 指定证书")?;
        Ok(PathBuf::from(home).join(".config/ccs-proxy/tls"))
    }
}

/// This never changes Codex configuration, account credentials or request paths.
pub fn prepare(directory: &Path) -> Result<Certificates> {
    prepare_with_store(directory, &SystemTrust)
}

fn prepare_with_store(directory: &Path, trust: &impl TrustStore) -> Result<Certificates> {
    let certs = ensure_certificates(directory)?;
    if !trust.is_trusted(&certs)? {
        info!(ca = %certs.ca.display(), "需要信任本地 HTTPS 证书，即将请求系统授权；取消将停止启动，不会降级为 HTTP");
        trust.request_trust(&certs)?;
        ensure!(
            trust.is_trusted(&certs)?,
            "系统尚未信任 localhost 证书。HTTPS 代理未启动，请重新运行并完成系统授权"
        );
    }
    Ok(certs)
}

fn ensure_certificates(directory: &Path) -> Result<Certificates> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    #[cfg(target_os = "windows")]
    platform::secure_directory(directory)?;
    let paths = Certificates {
        ca: directory.join("ca.pem"),
        cert: directory.join("localhost.pem"),
        key: directory.join("localhost-key.pem"),
    };
    let ca_key_path = directory.join("ca-key.pem");
    let now = OffsetDateTime::now_utc();
    let (ca_pem, ca_key) = if paths.ca.exists() || ca_key_path.exists() {
        ensure!(
            paths.ca.exists() && ca_key_path.exists(),
            "本地 CA 文件不完整，保留现有文件，请检查 {}",
            directory.display()
        );
        let pem = fs::read_to_string(&paths.ca)?;
        let der = CertificateDer::from_pem_slice(pem.as_bytes())?;
        let (_, parsed) = x509_parser::parse_x509_certificate(&der)
            .map_err(|e| anyhow::anyhow!("无效的 CA 证书: {e}"))?;
        let key = KeyPair::from_pem(&fs::read_to_string(&ca_key_path)?)?;
        ensure!(
            parsed.public_key().raw == key.subject_public_key_info(),
            "本地 CA 证书与私钥不匹配，未覆盖现有文件"
        );
        ensure!(parsed.is_ca(), "本地 CA 证书缺少 CA 属性");
        if parsed.validity().not_after.timestamp()
            <= now.unix_timestamp() + RENEW_BEFORE.as_secs() as i64
        {
            create_ca(&paths.ca, &ca_key_path, now)?
        } else {
            (pem, key)
        }
    } else {
        create_ca(&paths.ca, &ca_key_path, now)?
    };
    let ca_der = CertificateDer::from_pem_slice(ca_pem.as_bytes())?;
    // Verify hostname, signature, validity (including renewal window) and private key match.
    if !valid_leaf(&paths, ca_der.clone()) {
        let key = KeyPair::generate()?;
        let mut params =
            CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into(), "::1".into()])?;
        params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        params.not_before = now - time::Duration::minutes(5);
        params.not_after = now + time::Duration::days(365);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key)?;
        let cert = params.signed_by(&key, &issuer)?;
        write_private(&paths.key, key.serialize_pem().as_bytes())?;
        write_private(&paths.cert, cert.pem().as_bytes())?;
        info!(certificate = %paths.cert.display(), "已生成 localhost 服务端证书（有效期 365 天），复用本地 CA");
    }
    ensure!(valid_leaf(&paths, ca_der), "生成的 localhost 证书校验失败");
    Ok(paths)
}

fn create_ca(cert_path: &Path, key_path: &Path, now: OffsetDateTime) -> Result<(String, KeyPair)> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "CCS Proxy Local CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(3650);
    let pem = params.self_signed(&key)?.pem();
    write_private(key_path, key.serialize_pem().as_bytes())?;
    write_private(cert_path, pem.as_bytes())?;
    info!(ca = %cert_path.display(), "已生成本机专用 CA（有效期 10 年）");
    Ok((pem, key))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn valid_leaf(paths: &Certificates, ca: CertificateDer<'static>) -> bool {
    (|| -> Result<()> {
        let leaf = CertificateDer::from_pem_file(&paths.cert)?;
        let key = KeyPair::from_pem(&fs::read_to_string(&paths.key)?)?;
        let (_, parsed) =
            x509_parser::parse_x509_certificate(&leaf).map_err(|e| anyhow::anyhow!("{e}"))?;
        ensure!(
            parsed.public_key().raw == key.subject_public_key_info(),
            "key mismatch"
        );
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca)?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()?;
        use rustls::client::danger::ServerCertVerifier;
        for instant in [SystemTime::now(), SystemTime::now() + RENEW_BEFORE] {
            verifier.verify_server_cert(
                &leaf,
                &[],
                &ServerName::try_from("localhost")?,
                &[],
                UnixTime::since_unix_epoch(instant.duration_since(SystemTime::UNIX_EPOCH)?),
            )?;
        }
        Ok(())
    })()
    .is_ok()
}

trait TrustStore {
    fn is_trusted(&self, certs: &Certificates) -> Result<bool>;
    fn request_trust(&self, certs: &Certificates) -> Result<()>;
}
struct SystemTrust;

#[cfg(target_os = "macos")]
impl TrustStore for SystemTrust {
    fn is_trusted(&self, certs: &Certificates) -> Result<bool> {
        let output = std::process::Command::new("/usr/bin/security")
            .args([
                "verify-cert",
                "-q",
                "-L",
                "-p",
                "ssl",
                "-n",
                "localhost",
                "-c",
            ])
            .arg(&certs.cert)
            .arg("-c")
            .arg(&certs.ca)
            .output()
            .context("无法检查 macOS 证书信任")?;
        // Do not pass -r: an explicit root would bypass checking installed trust.
        Ok(output.status.success())
    }
    fn request_trust(&self, certs: &Certificates) -> Result<()> {
        let home = std::env::var_os("HOME").context("找不到用户钥匙串")?;
        let output = std::process::Command::new("/usr/bin/security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
            .arg(PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
            .arg(&certs.ca)
            .output()
            .context("无法请求 macOS 证书信任授权")?;
        ensure!(
            output.status.success(),
            "证书信任授权未完成，HTTPS 代理未启动：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
impl TrustStore for SystemTrust {
    fn is_trusted(&self, _certs: &Certificates) -> Result<bool> {
        anyhow::bail!(
            "自动系统信任支持 macOS、Linux 和 Windows；此系统请使用 --tls-cert 和 --tls-key"
        )
    }
    fn request_trust(&self, _certs: &Certificates) -> Result<()> {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct FakeTrust {
        trusted: Cell<bool>,
        requests: Cell<usize>,
        accept: bool,
    }
    impl TrustStore for FakeTrust {
        fn is_trusted(&self, _: &Certificates) -> Result<bool> {
            Ok(self.trusted.get())
        }
        fn request_trust(&self, _: &Certificates) -> Result<()> {
            self.requests.set(self.requests.get() + 1);
            ensure!(self.accept, "authorization cancelled");
            self.trusted.set(true);
            Ok(())
        }
    }
    #[test]
    fn first_start_requests_trust_and_subsequent_start_reuses_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let trust = FakeTrust {
            trusted: Cell::new(false),
            requests: Cell::new(0),
            accept: true,
        };
        let paths = prepare_with_store(dir.path(), &trust).unwrap();
        let ca = fs::read(&paths.ca).unwrap();
        let cert = fs::read(&paths.cert).unwrap();
        prepare_with_store(dir.path(), &trust).unwrap();
        assert_eq!(trust.requests.get(), 1);
        assert_eq!(fs::read(paths.ca).unwrap(), ca);
        assert_eq!(fs::read(paths.cert).unwrap(), cert);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(paths.key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn declined_authorization_stops_setup() {
        let dir = tempfile::tempdir().unwrap();
        let trust = FakeTrust {
            trusted: Cell::new(false),
            requests: Cell::new(0),
            accept: false,
        };
        assert!(prepare_with_store(dir.path(), &trust).is_err());
        assert_eq!(trust.requests.get(), 1);
    }
    #[test]
    fn missing_or_invalid_leaf_is_renewed_without_replacing_ca() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ensure_certificates(dir.path()).unwrap();
        let ca = fs::read(&paths.ca).unwrap();
        fs::write(&paths.cert, b"invalid").unwrap();
        ensure_certificates(dir.path()).unwrap();
        assert_eq!(fs::read(&paths.ca).unwrap(), ca);
        fs::remove_file(&paths.key).unwrap();
        ensure_certificates(dir.path()).unwrap();
        assert_eq!(fs::read(&paths.ca).unwrap(), ca);
    }
    #[test]
    fn soon_expiring_leaf_is_renewed_under_the_same_ca() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ensure_certificates(dir.path()).unwrap();
        let ca = fs::read_to_string(&paths.ca).unwrap();
        let ca_key =
            KeyPair::from_pem(&fs::read_to_string(dir.path().join("ca-key.pem")).unwrap()).unwrap();
        let issuer = Issuer::from_ca_cert_pem(&ca, ca_key).unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.not_before = OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + time::Duration::days(1);
        let old = params.signed_by(&key, &issuer).unwrap().pem();
        fs::write(&paths.cert, &old).unwrap();
        fs::write(&paths.key, key.serialize_pem()).unwrap();
        ensure_certificates(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(&paths.ca).unwrap(), ca);
        assert_ne!(fs::read_to_string(&paths.cert).unwrap(), old);
    }

    #[test]
    fn expired_ca_is_replaced_and_trust_is_requested_again() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        let (old, _) = create_ca(
            &ca_path,
            &dir.path().join("ca-key.pem"),
            OffsetDateTime::now_utc() - time::Duration::days(3651),
        )
        .unwrap();
        let trust = FakeTrust {
            trusted: Cell::new(false),
            requests: Cell::new(0),
            accept: true,
        };
        prepare_with_store(dir.path(), &trust).unwrap();
        assert_ne!(fs::read_to_string(ca_path).unwrap(), old);
        assert_eq!(trust.requests.get(), 1);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn system_trust_does_not_accept_an_uninstalled_ca() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ensure_certificates(dir.path()).unwrap();
        assert!(!SystemTrust.is_trusted(&paths).unwrap());
    }

    #[test]
    fn incomplete_ca_is_not_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ca.pem"), b"existing").unwrap();
        assert!(ensure_certificates(dir.path()).is_err());
        assert_eq!(fs::read(dir.path().join("ca.pem")).unwrap(), b"existing");
    }
}
