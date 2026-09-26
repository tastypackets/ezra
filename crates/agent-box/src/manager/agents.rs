use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::path_ext::PathExt;
use crate::process_ext::OutputExt;

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

impl fmt::Display for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.command_name())
    }
}

impl Agent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// The product name shown on the page.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    fn command_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn config_directory_variable(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
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
/// The config directories hold each CLI's sign-in, settings and sessions on the /config volume.
#[derive(Debug, Clone)]
pub struct InstallPaths {
    bin_directory: PathBuf,
    share_directory: PathBuf,
    claude_config_directory: Option<PathBuf>,
    codex_config_directory: Option<PathBuf>,
}

impl InstallPaths {
    pub fn under_home(home: &Path) -> Self {
        Self {
            bin_directory: home.join(".local/bin"),
            share_directory: home.join(".local/share"),
            claude_config_directory: None,
            codex_config_directory: None,
        }
    }

    pub fn with_config_directories_from_environment(self) -> Self {
        let from_environment =
            |agent: Agent| std::env::var_os(agent.config_directory_variable()).map(PathBuf::from);
        Self {
            claude_config_directory: from_environment(Agent::Claude),
            codex_config_directory: from_environment(Agent::Codex),
            ..self
        }
    }

    #[cfg(test)]
    pub fn with_config_directories(self, claude: PathBuf, codex: PathBuf) -> Self {
        Self {
            claude_config_directory: Some(claude),
            codex_config_directory: Some(codex),
            ..self
        }
    }

    pub fn config_directory(&self, agent: Agent) -> Option<&Path> {
        match agent {
            Agent::Claude => self.claude_config_directory.as_deref(),
            Agent::Codex => self.codex_config_directory.as_deref(),
        }
    }

    pub fn command(&self, agent: Agent) -> PathBuf {
        self.bin_directory.join(agent.command_name())
    }

    /// The version the command links to, or `None` when it is not installed.
    pub fn installed_version(&self, agent: Agent) -> Option<String> {
        let target = fs::read_link(self.command(agent)).ok()?;
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
    pub async fn install_latest(&self, agent: Agent) -> Result<String, InstallError> {
        let release = Release::latest(agent).await?;
        if self.installed_version(agent).as_deref() != Some(release.version.as_str()) {
            self.install(agent, &release).await?;
        }
        if let Some(config_directory) = self.config_directory(agent) {
            fs::create_dir_all(config_directory)?;
        }
        Ok(release.version)
    }

    async fn install(&self, agent: Agent, release: &Release) -> Result<(), InstallError> {
        let versions_directory = self.versions_directory(agent);
        fs::create_dir_all(&versions_directory)?;
        let download_path = versions_directory.join(format!(".{}.download", release.version));
        let staging_path = versions_directory.join(format!(".{}.partial", release.version));
        let version_path = versions_directory.join(&release.version);
        staging_path.remove_if_present()?;

        let outcome = async {
            Curl::download(&release.url, &download_path).await?;
            if download_path.sha256_hex()? != release.sha256 {
                return Err(InstallError::ChecksumMismatch {
                    url: release.url.clone(),
                });
            }
            match agent {
                Agent::Claude => {
                    fs::set_permissions(&download_path, fs::Permissions::from_mode(0o755))?;
                    fs::rename(&download_path, &staging_path)?;
                }
                Agent::Codex => Tar::extract_gzip(&download_path, &staging_path).await?,
            }
            version_path.remove_if_present()?;
            fs::rename(&staging_path, &version_path)?;
            Ok(())
        }
        .await;
        download_path.remove_if_present()?;
        staging_path.remove_if_present()?;
        outcome?;

        let command_target = match agent {
            Agent::Claude => version_path,
            Agent::Codex => version_path.join("bin/codex"),
        };
        self.command(agent).replace_symlink(&command_target)?;
        Self::remove_versions_except(&versions_directory, &release.version)?;
        Ok(())
    }

    fn versions_directory(&self, agent: Agent) -> PathBuf {
        match agent {
            Agent::Claude => self.share_directory.join("claude/versions"),
            Agent::Codex => self.share_directory.join("codex"),
        }
    }

    fn remove_versions_except(versions_directory: &Path, kept_version: &str) -> io::Result<()> {
        for path in versions_directory.entries_or_empty()? {
            if path.file_name().is_some_and(|name| name != kept_version) {
                path.remove_if_present()?;
            }
        }
        Ok(())
    }
}

/// One published build of a CLI for this platform.
#[derive(Debug, PartialEq, Eq)]
struct Release {
    version: String,
    url: String,
    sha256: String,
}

impl Release {
    async fn latest(agent: Agent) -> Result<Self, InstallError> {
        match agent {
            Agent::Claude => {
                let version = Curl::text(&format!("{CLAUDE_RELEASES}/latest"))
                    .await?
                    .trim()
                    .to_owned();
                let manifest_url = format!("{CLAUDE_RELEASES}/{version}/manifest.json");
                let manifest = Curl::text(&manifest_url).await?;
                Self::from_claude_manifest(&version, &manifest).map_err(|message| {
                    InstallError::ReleaseInformation {
                        url: manifest_url,
                        message,
                    }
                })
            }
            Agent::Codex => {
                let channel_url = format!("{CODEX_RELEASES}/channels/latest");
                let channel = Curl::text(&channel_url).await?;
                Self::from_codex_channel(&channel).map_err(|message| {
                    InstallError::ReleaseInformation {
                        url: channel_url,
                        message,
                    }
                })
            }
        }
    }

    fn from_claude_manifest(version: &str, manifest: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Manifest {
            platforms: HashMap<String, Platform>,
        }
        #[derive(Deserialize)]
        struct Platform {
            checksum: String,
        }

        let manifest: Manifest =
            serde_json::from_str(manifest).map_err(|error| error.to_string())?;
        let platform = manifest
            .platforms
            .get(CLAUDE_PLATFORM)
            .ok_or_else(|| format!("no {CLAUDE_PLATFORM} build"))?;
        Ok(Self {
            version: version.to_owned(),
            url: format!("{CLAUDE_RELEASES}/{version}/{CLAUDE_PLATFORM}/claude"),
            sha256: platform.checksum.to_ascii_lowercase(),
        })
    }

    fn from_codex_channel(channel: &str) -> Result<Self, String> {
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
        Ok(Self {
            version: version.to_owned(),
            url: asset.browser_download_url,
            sha256: sha256.to_ascii_lowercase(),
        })
    }
}

/// Downloads with the image's curl.
struct Curl;

impl Curl {
    async fn text(url: &str) -> Result<String, InstallError> {
        let output = Self::command().arg(url).output().await?;
        if !output.status.success() {
            return Err(InstallError::Download {
                url: url.to_owned(),
                message: output.stderr_text(),
            });
        }
        Ok(output.stdout_text())
    }

    async fn download(url: &str, destination: &Path) -> Result<(), InstallError> {
        let output = Self::command()
            .arg("--output")
            .arg(destination)
            .arg(url)
            .output()
            .await?;
        if !output.status.success() {
            return Err(InstallError::Download {
                url: url.to_owned(),
                message: output.stderr_text(),
            });
        }
        Ok(())
    }

    fn command() -> Command {
        let mut command = Command::new("curl");
        command.args(["--fail", "--silent", "--show-error", "--location"]);
        command
    }
}

/// Unpacks archives with the image's tar.
struct Tar;

impl Tar {
    async fn extract_gzip(archive: &Path, destination: &Path) -> Result<(), InstallError> {
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
                message: output.stderr_text(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_file(path: &Path) {
        fs::create_dir_all(path.parent().expect("test paths have a parent"))
            .expect("parent directory is created");
        fs::write(path, "").expect("test file is written");
    }

    #[test]
    fn claude_release_comes_from_the_manifest() {
        let manifest = r#"{"version":"2.1.283","platforms":{
            "linux-x64":{"binary":"claude","checksum":"ABC123","size":1},
            "darwin-arm64":{"binary":"claude","checksum":"def456","size":1}}}"#;
        assert_eq!(
            Release::from_claude_manifest("2.1.283", manifest).expect("manifest parses"),
            Release {
                version: "2.1.283".to_owned(),
                url: format!("{CLAUDE_RELEASES}/2.1.283/linux-x64/claude"),
                sha256: "abc123".to_owned(),
            }
        );
        assert!(Release::from_claude_manifest("2.1.283", r#"{"platforms":{}}"#).is_err());
    }

    #[test]
    fn codex_release_comes_from_the_channel() {
        let channel = format!(
            r#"{{"tag_name":"rust-v0.157.1","assets":[
                {{"name":"codex-x86_64-unknown-linux-musl.tar.gz","digest":"sha256:111","browser_download_url":"https://example/single"}},
                {{"name":"{CODEX_PACKAGE}","digest":"sha256:222","browser_download_url":"https://example/package"}}]}}"#
        );
        assert_eq!(
            Release::from_codex_channel(&channel).expect("channel parses"),
            Release {
                version: "0.157.1".to_owned(),
                url: "https://example/package".to_owned(),
                sha256: "222".to_owned(),
            }
        );
        assert!(Release::from_codex_channel(r#"{"tag_name":"v1","assets":[]}"#).is_err());
    }

    #[test]
    fn installed_version_follows_the_command_link() {
        let home = tempfile::tempdir().expect("temporary directory");
        let paths = InstallPaths::under_home(home.path());
        assert_eq!(paths.installed_version(Agent::Claude), None);

        let claude_binary = paths.versions_directory(Agent::Claude).join("2.1.283");
        create_file(&claude_binary);
        paths
            .command(Agent::Claude)
            .replace_symlink(&claude_binary)
            .expect("command link is created");
        assert_eq!(
            paths.installed_version(Agent::Claude).as_deref(),
            Some("2.1.283")
        );

        let codex_binary = paths
            .versions_directory(Agent::Codex)
            .join("0.157.1/bin/codex");
        create_file(&codex_binary);
        paths
            .command(Agent::Codex)
            .replace_symlink(&codex_binary)
            .expect("command link is created");
        assert_eq!(
            paths.installed_version(Agent::Codex).as_deref(),
            Some("0.157.1")
        );
    }

    #[test]
    fn dangling_command_link_means_not_installed() {
        let home = tempfile::tempdir().expect("temporary directory");
        let paths = InstallPaths::under_home(home.path());
        paths
            .command(Agent::Claude)
            .replace_symlink(Path::new("/missing/2.1.0"))
            .expect("command link is created");
        assert_eq!(paths.installed_version(Agent::Claude), None);
    }

    #[test]
    fn only_the_current_version_is_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for name in ["2.1.1", "2.1.2", ".2.1.3.partial"] {
            fs::create_dir(directory.path().join(name)).expect("version directory is created");
        }
        InstallPaths::remove_versions_except(directory.path(), "2.1.2")
            .expect("old versions are removed");
        let remaining: Vec<_> = directory
            .path()
            .entries_or_empty()
            .expect("directory is readable")
            .into_iter()
            .filter_map(|path| path.file_name().map(ToOwned::to_owned))
            .collect();
        assert_eq!(remaining, ["2.1.2"]);
    }
}
