#[cfg(any(target_os = "linux", target_os = "windows"))]
use super::{Certificates, TrustStore};
use anyhow::Result;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use anyhow::{Context, ensure};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::Command;

#[cfg(target_os = "windows")]
pub(super) fn secure_directory(directory: &Path) -> Result<()> {
    let output = powershell(r#"
$path = $env:CCS_TLS_DIRECTORY
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = [System.Security.AccessControl.DirectorySecurity]::new()
$acl.SetAccessRuleProtection($true, $false)
$rule = [System.Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
$acl.AddAccessRule($rule)
[System.IO.Directory]::SetAccessControl($path, $acl)
"#).env("CCS_TLS_DIRECTORY", directory).output()?;
    ensure!(
        output.status.success(),
        "无法保护 Windows 证书私钥目录: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn powershell(script: &str) -> Command {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("$ErrorActionPreference = 'Stop'; {script}"));
    cmd
}

#[cfg(target_os = "windows")]
fn windows_command(script: &str, certs: &Certificates) -> Result<Command> {
    use base64::Engine;
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let mut cmd = powershell(script);
    cmd.env(
        "CCS_TLS_CA",
        base64::engine::general_purpose::STANDARD.encode(CertificateDer::from_pem_file(&certs.ca)?),
    );
    cmd.env(
        "CCS_TLS_LEAF",
        base64::engine::general_purpose::STANDARD
            .encode(CertificateDer::from_pem_file(&certs.cert)?),
    );
    Ok(cmd)
}

#[cfg(target_os = "windows")]
impl TrustStore for super::SystemTrust {
    fn is_trusted(&self, certs: &Certificates) -> Result<bool> {
        let output = windows_command(r#"
$ca = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new([Convert]::FromBase64String($env:CCS_TLS_CA))
$leaf = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new([Convert]::FromBase64String($env:CCS_TLS_LEAF))
$chain = [System.Security.Cryptography.X509Certificates.X509Chain]::new()
$chain.ChainPolicy.RevocationMode = 'NoCheck'
$chain.ChainPolicy.VerificationFlags = 'NoFlag'
$chain.ChainPolicy.ApplicationPolicy.Add(([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1')))
[void]$chain.ChainPolicy.ExtraStore.Add($ca)
$ok = $chain.Build($leaf)
if ($ok -and $chain.ChainElements[$chain.ChainElements.Count - 1].Certificate.Thumbprint -eq $ca.Thumbprint) { exit 0 }
exit 10
"#, certs)?.output().context("无法运行 Windows PowerShell 证书验证")?;
        ensure!(
            output.status.success() || output.status.code() == Some(10),
            "Windows 证书信任检查失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.status.success())
    }
    fn request_trust(&self, certs: &Certificates) -> Result<()> {
        let output = windows_command(r#"
$ca = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new([Convert]::FromBase64String($env:CCS_TLS_CA))
Add-Type -AssemblyName PresentationFramework
$message = 'CCS Proxy needs to trust its local CA to serve https://localhost:8000.' + [Environment]::NewLine + 'Only the current user certificate store is modified.' + [Environment]::NewLine + 'SHA-1: ' + $ca.Thumbprint
if ([System.Windows.MessageBox]::Show($message, 'CCS Proxy - Local HTTPS Certificate', 'YesNo', 'Question') -ne 'Yes') { exit 10 }
$store = [System.Security.Cryptography.X509Certificates.X509Store]::new('Root', 'CurrentUser')
try { $store.Open('ReadWrite'); $store.Add($ca) } finally { $store.Close() }
"#, certs)?.output().context("无法请求 Windows 证书授权")?;
        ensure!(
            output.status.success(),
            "Windows 证书信任授权被取消或安装失败，HTTPS 代理未启动: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq)]
struct LinuxStore {
    directory: PathBuf,
    update: PathBuf,
    args: Vec<&'static str>,
}

#[cfg(any(target_os = "linux", test))]
fn linux_store(root: &Path) -> Result<LinuxStore> {
    for (tool, directory, args) in [
        (
            "usr/sbin/update-ca-certificates",
            "usr/local/share/ca-certificates",
            vec![],
        ),
        (
            "usr/bin/update-ca-trust",
            "etc/pki/ca-trust/source/anchors",
            vec!["extract"],
        ),
        (
            "usr/sbin/update-ca-trust",
            "etc/pki/ca-trust/source/anchors",
            vec!["extract"],
        ),
    ] {
        if root.join(tool).is_file() {
            let directory = if tool.ends_with("update-ca-trust")
                && root
                    .join("etc/ca-certificates/trust-source/anchors")
                    .is_dir()
            {
                "etc/ca-certificates/trust-source/anchors"
            } else if tool.ends_with("update-ca-certificates")
                && root.join("etc/pki/trust/anchors").is_dir()
            {
                "etc/pki/trust/anchors"
            } else {
                directory
            };
            return Ok(LinuxStore {
                directory: root.join(directory),
                update: root.join(tool),
                args,
            });
        }
    }
    anyhow::bail!(
        "找不到 update-ca-certificates / update-ca-trust，请先安装发行版的 ca-certificates 包"
    )
}

#[cfg(target_os = "linux")]
fn nss_databases() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return vec![];
    };
    [".pki/nssdb", ".local/share/pki/nssdb"]
        .into_iter()
        .map(|path| PathBuf::from(&home).join(path))
        .filter(|path| path.join("cert9.db").is_file())
        .collect()
}

#[cfg(target_os = "linux")]
fn ca_name(certs: &Certificates) -> Result<String> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    use sha2::{Digest, Sha256};
    Ok(format!(
        "ccs-proxy-{:x}",
        Sha256::digest(CertificateDer::from_pem_file(&certs.ca)?)
    ))
}

#[cfg(target_os = "linux")]
fn nss_trusted(certs: &Certificates, database: &Path) -> Result<bool> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let name = ca_name(certs)?;
    let db = format!("sql:{}", database.display());
    let cert = Command::new("certutil").args(["-L", "-d", &db, "-n", &name, "-a"]).output()
        .context("浏览器使用 NSS 信任库，请安装 libnss3-tools（Debian/Ubuntu）或 nss-tools（Fedora/RHEL）后重试")?;
    if !cert.status.success() {
        return Ok(false);
    }
    let der = CertificateDer::from_pem_slice(&cert.stdout)?;
    if der != CertificateDer::from_pem_file(&certs.ca)? {
        return Ok(false);
    }
    let list = Command::new("certutil").args(["-L", "-d", &db]).output()?;
    ensure!(list.status.success(), "无法读取 NSS 信任标记");
    Ok(String::from_utf8_lossy(&list.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(name.as_str())
            && fields
                .next()
                .is_some_and(|flags| flags.split(',').next().unwrap_or_default().contains('C'))
    }))
}

#[cfg(target_os = "linux")]
impl TrustStore for super::SystemTrust {
    fn is_trusted(&self, certs: &Certificates) -> Result<bool> {
        let result = Command::new("openssl")
            .env_remove("SSL_CERT_FILE")
            .env_remove("SSL_CERT_DIR")
            .args([
                "verify",
                "-purpose",
                "sslserver",
                "-verify_hostname",
                "localhost",
                "-untrusted",
            ])
            .arg(&certs.ca)
            .arg(&certs.cert)
            .output()
            .context("需要 OpenSSL 来验证 Linux 系统信任，请安装 openssl 包")?;
        if !result.status.success() {
            return Ok(false);
        }
        for db in nss_databases() {
            if !nss_trusted(certs, &db)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn request_trust(&self, certs: &Certificates) -> Result<()> {
        let store = linux_store(Path::new("/"))?;
        // Check browser dependencies before changing the system store.
        let databases = nss_databases();
        for db in &databases {
            let _ = nss_trusted(certs, db)?;
        }
        let destination = store.directory.join(format!("{}.crt", ca_name(certs)?));
        let root = Command::new("id").arg("-u").output()?;
        ensure!(root.status.success(), "无法确认 Linux 用户权限");
        let is_root = root.stdout == b"0\n";
        let graphical =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        let mut command = if is_root {
            Command::new("/bin/sh")
        } else if graphical && executable_in_path("pkexec") {
            let mut cmd = Command::new("pkexec");
            cmd.arg("/bin/sh");
            cmd
        } else {
            ensure!(
                executable_in_path("sudo"),
                "需要 pkexec（桌面）或 sudo（终端）来授权安装系统 CA"
            );
            let mut cmd = Command::new("sudo");
            cmd.arg("--").arg("/bin/sh");
            cmd
        };
        // Fixed script, all paths passed as positional arguments; never interpolate shell code.
        let status = command.args(["-c", "set -eu; source=$1; destination=$2; shift 2; install -D -m 0644 -- \"$source\" \"$destination\"; exec \"$@\"", "ccs-proxy-trust"])
            .arg(&certs.ca).arg(destination).arg(&store.update).args(&store.args)
            .status().context("无法请求 Linux 系统授权")?;
        ensure!(
            status.success(),
            "Linux 证书授权被取消或信任库更新失败，HTTPS 代理未启动"
        );
        for db in databases {
            let status = Command::new("certutil")
                .args(["-A", "-d"])
                .arg(format!("sql:{}", db.display()))
                .args(["-n", &ca_name(certs)?, "-t", "C,,", "-i"])
                .arg(&certs.ca)
                .status()?;
            ensure!(
                status.success(),
                "NSS 浏览器信任库安装失败，HTTPS 代理未启动"
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn executable_in_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chooses_debian_and_redhat_trust_stores() {
        for (tool, destination, args) in [
            (
                "usr/sbin/update-ca-certificates",
                "usr/local/share/ca-certificates",
                vec![],
            ),
            (
                "usr/bin/update-ca-trust",
                "etc/pki/ca-trust/source/anchors",
                vec!["extract"],
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(tool);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
            let store = linux_store(root.path()).unwrap();
            assert_eq!(store.directory, root.path().join(destination));
            assert_eq!(store.args, args);
        }
    }
    #[test]
    fn missing_linux_trust_tools_is_an_explicit_error() {
        let root = tempfile::tempdir().unwrap();
        assert!(linux_store(root.path()).is_err());
    }
}
