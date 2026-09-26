use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;

const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";
const CLAUDE_PLATFORM: &str = "linux-x64";
const CODEX_RELEASES: &str = "https://releases.openai.com/codex";
const CODEX_PACKAGE: &str = "codex-package-x86_64-unknown-linux-musl.tar.gz";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl std::fmt::Display for Agent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.command_name())
    }
}

impl Agent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    fn config_directory_variable(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        }
    }

    fn command_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not download {url}: {message}")]
    Download { url: String, message: String },
    #[error("unexpected release information from {url}: {message}")]
    ReleaseInformation { url: String, message: String },
    #[error("{url} did not match its published SHA-256 checksum")]
    ChecksumMismatch { url: String },
    #[error("could not unpack {archive}: {message}")]
    Unpack { archive: String, message: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// `~/.local/bin` holds the commands; `~/.local/share/<agent>` holds one directory or file per version.
#[derive(Debug, Clone)]
pub struct InstallPaths {
    bin_directory: PathBuf,
    share_directory: PathBuf,
}

impl InstallPaths {
    pub fn under_home(home: &Path) -> Self {
        Self {
            bin_directory: home.join(".local/bin"),
            share_directory: home.join(".local/share"),
        }
    }

    fn command(&self, agent: Agent) -> PathBuf {
        self.bin_directory.join(agent.command_name())
    }

    fn versions_directory(&self, agent: Agent) -> PathBuf {
        match agent {
            Agent::Claude => self.share_directory.join("claude/versions"),
            Agent::Codex => self.share_directory.join("codex"),
        }
    }
}

/// The version the command links to, or `None` when it is not installed.
pub fn installed_version(agent: Agent, paths: &InstallPaths) -> Option<String> {
    let target = fs::read_link(paths.command(agent)).ok()?;
    if !target.exists() {
        return None;
    }
    let version_path = match agent {
        Agent::Claude => target.as_path(),
        Agent::Codex => target.parent()?.parent()?,
    };
    Some(version_path.file_name()?.to_string_lossy().into_owned())
}

/// Installs the newest release, or does nothing when it is already installed. Returns the installed version.
pub async fn install_latest(agent: Agent, paths: &InstallPaths) -> Result<String, InstallError> {
    let release = latest_release(agent).await?;
    if installed_version(agent, paths).as_deref() == Some(release.version.as_str()) {
        return Ok(release.version);
    }

    let versions_directory = paths.versions_directory(agent);
    fs::create_dir_all(&versions_directory)?;
    let download_path = versions_directory.join(format!(".{}.download", release.version));
    let staging_path = versions_directory.join(format!(".{}.partial", release.version));
    let version_path = versions_directory.join(&release.version);
    remove_path(&staging_path)?;

    let outcome = async {
        download(&release.url, &download_path).await?;
        if sha256_of(&download_path)? != release.sha256 {
            return Err(InstallError::ChecksumMismatch { url: release.url });
        }
        match agent {
            Agent::Claude => {
                fs::set_permissions(&download_path, fs::Permissions::from_mode(0o755))?;
                fs::rename(&download_path, &staging_path)?;
            }
            Agent::Codex => unpack(&download_path, &staging_path).await?,
        }
        remove_path(&version_path)?;
        fs::rename(&staging_path, &version_path)?;
        Ok(())
    }
    .await;
    remove_path(&download_path)?;
    remove_path(&staging_path)?;
    outcome?;

    let command_target = match agent {
        Agent::Claude => version_path.clone(),
        Agent::Codex => version_path.join("bin/codex"),
    };
    link_command(&paths.command(agent), &command_target)?;
    remove_other_versions(&versions_directory, &release.version)?;
    if let Some(config_directory) = std::env::var_os(agent.config_directory_variable()) {
        fs::create_dir_all(config_directory)?;
    }
    Ok(release.version)
}

#[derive(Debug, PartialEq, Eq)]
struct Release {
    version: String,
    url: String,
    sha256: String,
}

async fn latest_release(agent: Agent) -> Result<Release, InstallError> {
    match agent {
        Agent::Claude => {
            let version = fetch_text(&format!("{CLAUDE_RELEASES}/latest"))
                .await?
                .trim()
                .to_owned();
            let manifest_url = format!("{CLAUDE_RELEASES}/{version}/manifest.json");
            let manifest = fetch_text(&manifest_url).await?;
            claude_release(&version, &manifest).map_err(|message| {
                InstallError::ReleaseInformation {
                    url: manifest_url,
                    message,
                }
            })
        }
        Agent::Codex => {
            let channel_url = format!("{CODEX_RELEASES}/channels/latest");
            let channel = fetch_text(&channel_url).await?;
            codex_release(&channel).map_err(|message| InstallError::ReleaseInformation {
                url: channel_url,
                message,
            })
        }
    }
}

fn claude_release(version: &str, manifest: &str) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Manifest {
        platforms: std::collections::HashMap<String, Platform>,
    }
    #[derive(Deserialize)]
    struct Platform {
        checksum: String,
    }

    let manifest: Manifest = serde_json::from_str(manifest).map_err(|error| error.to_string())?;
    let platform = manifest
        .platforms
        .get(CLAUDE_PLATFORM)
        .ok_or_else(|| format!("no {CLAUDE_PLATFORM} build"))?;
    Ok(Release {
        version: version.to_owned(),
        url: format!("{CLAUDE_RELEASES}/{version}/{CLAUDE_PLATFORM}/claude"),
        sha256: platform.checksum.to_ascii_lowercase(),
    })
}

fn codex_release(channel: &str) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Channel {
        tag_name: String,
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        digest: String,
        browser_download_url: String,
    }

    let channel: Channel = serde_json::from_str(channel).map_err(|error| error.to_string())?;
    let version = channel
        .tag_name
        .strip_prefix("rust-v")
        .ok_or_else(|| format!("unexpected tag {:?}", channel.tag_name))?;
    let asset = channel
        .assets
        .into_iter()
        .find(|asset| asset.name == CODEX_PACKAGE)
        .ok_or_else(|| format!("no {CODEX_PACKAGE} asset"))?;
    let sha256 = asset
        .digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("unexpected digest {:?}", asset.digest))?;
    Ok(Release {
        version: version.to_owned(),
        url: asset.browser_download_url,
        sha256: sha256.to_ascii_lowercase(),
    })
}

async fn fetch_text(url: &str) -> Result<String, InstallError> {
    let output = curl().arg(url).output().await?;
    if !output.status.success() {
        return Err(InstallError::Download {
            url: url.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn download(url: &str, destination: &Path) -> Result<(), InstallError> {
    let output = curl()
        .arg("--output")
        .arg(destination)
        .arg(url)
        .output()
        .await?;
    if !output.status.success() {
        return Err(InstallError::Download {
            url: url.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn curl() -> Command {
    let mut command = Command::new("curl");
    command.args(["--fail", "--silent", "--show-error", "--location"]);
    command
}

fn sha256_of(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

async fn unpack(archive: &Path, destination: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(destination)?;
    let output = Command::new("tar")
        .arg("--extract")
        .arg("--gzip")
        .arg("--file")
        .arg(archive)
        .arg("--directory")
        .arg(destination)
        .output()
        .await?;
    if !output.status.success() {
        return Err(InstallError::Unpack {
            archive: archive.display().to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

/// Replaces the command link in one step, so the command never disappears.
fn link_command(command: &Path, target: &Path) -> io::Result<()> {
    let directory = command.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let staging_link = directory.join(format!(
        ".{}.new",
        command.file_name().unwrap_or_default().to_string_lossy()
    ));
    remove_path(&staging_link)?;
    symlink(target, &staging_link)?;
    fs::rename(&staging_link, command)
}

fn remove_other_versions(versions_directory: &Path, current_version: &str) -> io::Result<()> {
    for entry in fs::read_dir(versions_directory)? {
        let path = entry?.path();
        if path.file_name().is_some_and(|name| name != current_version) {
            remove_path(&path)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    let outcome = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match outcome {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        outcome => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_release_comes_from_the_manifest() {
        let manifest = r#"{"version":"2.1.283","platforms":{
            "linux-x64":{"binary":"claude","checksum":"ABC123","size":1},
            "darwin-arm64":{"binary":"claude","checksum":"def456","size":1}}}"#;
        assert_eq!(
            claude_release("2.1.283", manifest).unwrap(),
            Release {
                version: "2.1.283".to_owned(),
                url: format!("{CLAUDE_RELEASES}/2.1.283/linux-x64/claude"),
                sha256: "abc123".to_owned(),
            }
        );
        assert!(claude_release("2.1.283", r#"{"platforms":{}}"#).is_err());
    }

    #[test]
    fn codex_release_comes_from_the_channel() {
        let channel = format!(
            r#"{{"tag_name":"rust-v0.157.1","assets":[
                {{"name":"codex-x86_64-unknown-linux-musl.tar.gz","digest":"sha256:111","browser_download_url":"https://example/single"}},
                {{"name":"{CODEX_PACKAGE}","digest":"sha256:222","browser_download_url":"https://example/package"}}]}}"#
        );
        assert_eq!(
            codex_release(&channel).unwrap(),
            Release {
                version: "0.157.1".to_owned(),
                url: "https://example/package".to_owned(),
                sha256: "222".to_owned(),
            }
        );
        assert!(codex_release(r#"{"tag_name":"v1","assets":[]}"#).is_err());
    }

    #[test]
    fn installed_version_follows_the_command_link() {
        let home = tempfile::tempdir().unwrap();
        let paths = InstallPaths::under_home(home.path());
        assert_eq!(installed_version(Agent::Claude, &paths), None);

        let claude_binary = paths.versions_directory(Agent::Claude).join("2.1.283");
        fs::create_dir_all(claude_binary.parent().unwrap()).unwrap();
        fs::write(&claude_binary, "").unwrap();
        link_command(&paths.command(Agent::Claude), &claude_binary).unwrap();
        assert_eq!(
            installed_version(Agent::Claude, &paths).as_deref(),
            Some("2.1.283")
        );

        let codex_binary = paths
            .versions_directory(Agent::Codex)
            .join("0.157.1/bin/codex");
        fs::create_dir_all(codex_binary.parent().unwrap()).unwrap();
        fs::write(&codex_binary, "").unwrap();
        link_command(&paths.command(Agent::Codex), &codex_binary).unwrap();
        assert_eq!(
            installed_version(Agent::Codex, &paths).as_deref(),
            Some("0.157.1")
        );
    }

    #[test]
    fn dangling_command_link_means_not_installed() {
        let home = tempfile::tempdir().unwrap();
        let paths = InstallPaths::under_home(home.path());
        link_command(&paths.command(Agent::Claude), Path::new("/missing/2.1.0")).unwrap();
        assert_eq!(installed_version(Agent::Claude, &paths), None);
    }

    #[test]
    fn only_the_current_version_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["2.1.1", "2.1.2", ".2.1.3.partial"] {
            fs::create_dir(directory.path().join(name)).unwrap();
        }
        remove_other_versions(directory.path(), "2.1.2").unwrap();
        let remaining: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(remaining, ["2.1.2"]);
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        fs::write(&path, "abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
