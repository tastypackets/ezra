use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;

use super::processes::Process;
use crate::bytes_ext::BytesExt;
use crate::path_ext::PathExt;

const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";
#[cfg(target_arch = "x86_64")]
const CLAUDE_PLATFORM: &str = "linux-x64";
#[cfg(target_arch = "aarch64")]
const CLAUDE_PLATFORM: &str = "linux-arm64";
const CODEX_RELEASES: &str = "https://releases.openai.com/codex";
#[cfg(target_arch = "x86_64")]
const CODEX_PACKAGE: &str = "codex-package-x86_64-unknown-linux-musl.tar.gz";
#[cfg(target_arch = "aarch64")]
const CODEX_PACKAGE: &str = "codex-package-aarch64-unknown-linux-musl.tar.gz";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub use ezra::agent::Agent;

pub trait AgentExt {
    fn latest_version(
        self,
        channel: ReleaseChannel,
        tls_verification: TlsVerification,
    ) -> impl Future<Output = Result<String, InstallError>> + Send;
}

impl AgentExt for Agent {
    async fn latest_version(
        self,
        channel: ReleaseChannel,
        tls_verification: TlsVerification,
    ) -> Result<String, InstallError> {
        let client = ReleaseClient::new(tls_verification)?;
        Ok(Release::latest(self, channel, &client).await?.version)
    }
}

/// Which Claude Code releases to install: `latest` gets every release as soon as it ships,
/// `stable` is about a week behind and skips releases with major regressions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChannel {
    #[default]
    Latest,
    Stable,
}

impl ReleaseChannel {
    fn name(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Stable => "stable",
        }
    }
}

pub trait VersionExt {
    /// Semantic version order, or any difference when either is not a semantic version.
    fn is_newer_than(&self, installed: &str) -> bool;
}

impl VersionExt for str {
    fn is_newer_than(&self, installed: &str) -> bool {
        match (Version::parse(self), Version::parse(installed)) {
            (Ok(latest), Ok(installed)) => latest > installed,
            _ => self != installed,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not download {url}: {source}")]
    Download { url: String, source: reqwest::Error },
    #[error("unexpected release information from {url}: {message}")]
    ReleaseInformation { url: String, message: String },
    #[error("{url} did not match its published SHA-256 checksum")]
    ChecksumMismatch { url: String },
    #[error("could not unpack {archive}: {source}")]
    Unpack { archive: String, source: io::Error },
    #[error("could not start the download client: {0}")]
    Client(reqwest::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// `~/.local/bin` holds the commands. The versions directory holds one directory or file per
/// version of each agent, /cache/agents in the image. The config directories hold each CLI's
/// sign-in, settings and sessions.
#[derive(Debug, Clone)]
pub struct InstallPaths {
    home: PathBuf,
    bin_directory: PathBuf,
    versions_root: PathBuf,
    claude_config_directory: Option<PathBuf>,
    codex_config_directory: Option<PathBuf>,
}

impl InstallPaths {
    pub fn under_home(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            bin_directory: home.join(".local/bin"),
            versions_root: home.join(".local/share"),
            claude_config_directory: None,
            codex_config_directory: None,
        }
    }

    pub fn with_versions_in(self, versions_root: PathBuf) -> Self {
        Self {
            versions_root,
            ..self
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

    /// The home directory the agents run with.
    pub fn home(&self) -> &Path {
        &self.home
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

    /// The file the command links to and its version, or `None` when it is not installed.
    pub fn installed_command(&self, agent: Agent) -> Option<(PathBuf, String)> {
        let target = fs::read_link(self.command(agent)).ok()?;
        if !target.exists() {
            return None;
        }
        let version_path = match agent {
            Agent::Claude => target.as_path(),
            Agent::Codex => target.parent()?.parent()?,
        };
        let version = version_path.file_name()?.to_string_lossy().into_owned();
        Some((target, version))
    }

    /// The version the command links to, or `None` when it is not installed.
    pub fn installed_version(&self, agent: Agent) -> Option<String> {
        self.installed_command(agent).map(|(_, version)| version)
    }

    /// Links a missing command to the newest version already kept, and returns that version.
    pub fn link_kept_version(&self, agent: Agent) -> io::Result<Option<String>> {
        let kept = self
            .versions_directory(agent)
            .entries_or_empty()?
            .into_iter()
            .filter_map(|path| {
                let version = path.file_name()?.to_str()?.to_owned();
                (!version.starts_with('.')).then_some((version, path))
            })
            .reduce(|newest, candidate| {
                if candidate.0.is_newer_than(&newest.0) {
                    candidate
                } else {
                    newest
                }
            });
        let Some((version, path)) = kept else {
            return Ok(None);
        };
        self.link(agent, &path)?;
        Ok(Some(version))
    }

    /// Points the command at the release unpacked at `version_path`.
    pub fn link(&self, agent: Agent, version_path: &Path) -> io::Result<()> {
        self.command(agent)
            .replace_symlink(&agent.command_in(version_path))
    }

    /// Installs the channel's release when it is newer than the installed one. Returns the
    /// installed version.
    pub async fn install_latest(
        &self,
        agent: Agent,
        channel: ReleaseChannel,
        tls_verification: TlsVerification,
        progress: &InstallProgress,
    ) -> Result<String, InstallError> {
        let client = ReleaseClient::new(tls_verification)?;
        let release = Release::latest(agent, channel, &client).await?;
        let installed_version = match self.installed_version(agent) {
            Some(installed) if !release.version.is_newer_than(&installed) => installed,
            _ => {
                self.install(agent, &release, &client, progress).await?;
                release.version
            }
        };
        if let Some(config_directory) = self.config_directory(agent) {
            fs::create_dir_all(config_directory)?;
        }
        Ok(installed_version)
    }

    async fn install(
        &self,
        agent: Agent,
        release: &Release,
        client: &ReleaseClient,
        progress: &InstallProgress,
    ) -> Result<(), InstallError> {
        let versions_directory = self.versions_directory(agent);
        fs::create_dir_all(&versions_directory)?;
        let download_path = versions_directory.join(format!(".{}.download", release.version));
        let staging_path = versions_directory.join(format!(".{}.partial", release.version));
        let version_path = versions_directory.join(&release.version);
        if version_path.exists() {
            self.link(agent, &version_path)?;
            return self
                .remove_unused_versions(agent)
                .map_err(InstallError::from);
        }
        staging_path.remove_if_present()?;

        let outcome = async {
            let sha256 = client
                .download(&release.url, &download_path, progress)
                .await?;
            if sha256 != release.sha256 {
                return Err(InstallError::ChecksumMismatch {
                    url: release.url.clone(),
                });
            }
            match agent {
                Agent::Claude => {
                    fs::set_permissions(&download_path, fs::Permissions::from_mode(0o755))?;
                    fs::rename(&download_path, &staging_path)?;
                }
                Agent::Codex => {
                    let (archive, destination) = (download_path.clone(), staging_path.clone());
                    tokio::task::spawn_blocking(move || archive.unpack_tar_gz_into(&destination))
                        .await
                        .map_err(io::Error::other)?
                        .map_err(|source| InstallError::Unpack {
                            archive: download_path.display().to_string(),
                            source,
                        })?;
                }
            }
            version_path.remove_if_present()?;
            fs::rename(&staging_path, &version_path)?;
            Ok(())
        }
        .await;
        download_path.remove_if_present()?;
        staging_path.remove_if_present()?;
        outcome?;

        self.link(agent, &version_path)?;
        self.remove_unused_versions(agent)?;
        Ok(())
    }

    /// Removes versions other than the installed one, keeping any a process still runs.
    pub fn remove_unused_versions(&self, agent: Agent) -> io::Result<()> {
        let Some(installed) = self.installed_version(agent) else {
            return Ok(());
        };
        let versions_directory = self.versions_directory(agent);
        let mut kept = Process::running_from(&versions_directory);
        kept.push(installed);
        Self::remove_versions_except(&versions_directory, &kept)
    }

    /// Removes the command and every kept version. Blocks.
    pub fn uninstall(&self, agent: Agent) -> io::Result<()> {
        self.command(agent).remove_if_present()?;
        self.versions_directory(agent).remove_if_present()
    }

    /// Removes everything the agent saved, keeping its directory so a bind mount stays usable.
    /// Blocks.
    pub fn remove_saved_data(&self, agent: Agent) -> io::Result<()> {
        let Some(directory) = self.config_directory(agent) else {
            return Ok(());
        };
        for entry in directory.entries_or_empty()? {
            entry.remove_if_present()?;
        }
        Ok(())
    }

    pub fn versions_directory(&self, agent: Agent) -> PathBuf {
        self.versions_root.join(agent.command_name())
    }

    fn remove_versions_except(versions_directory: &Path, kept: &[String]) -> io::Result<()> {
        for path in versions_directory.entries_or_empty()? {
            if path
                .file_name()
                .is_some_and(|name| !kept.iter().any(|kept| name == kept.as_str()))
            {
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
    async fn latest(
        agent: Agent,
        channel: ReleaseChannel,
        client: &ReleaseClient,
    ) -> Result<Self, InstallError> {
        match agent {
            Agent::Claude => {
                let version = client
                    .text(&format!("{CLAUDE_RELEASES}/{}", channel.name()))
                    .await?
                    .trim()
                    .to_owned();
                let manifest_url = format!("{CLAUDE_RELEASES}/{version}/manifest.json");
                let manifest = client.text(&manifest_url).await?;
                Self::from_claude_manifest(&version, &manifest).map_err(|message| {
                    InstallError::ReleaseInformation {
                        url: manifest_url,
                        message,
                    }
                })
            }
            Agent::Codex => {
                let channel_url = format!("{CODEX_RELEASES}/channels/latest");
                let channel = client.text(&channel_url).await?;
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

/// Whether downloads check the server's certificate, from `EZRA_TLS_VERIFY`.
///
/// Off only for networks that intercept TLS without a CA the image trusts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsVerification {
    #[default]
    On,
    Off,
}

/// Talks to the vendors' release servers.
struct ReleaseClient {
    http: reqwest::Client,
}

impl ReleaseClient {
    fn new(tls_verification: TlsVerification) -> Result<Self, InstallError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .tls_danger_accept_invalid_certs(tls_verification == TlsVerification::Off)
            .build()
            .map_err(InstallError::Client)?;
        Ok(Self { http })
    }

    async fn text(&self, url: &str) -> Result<String, InstallError> {
        let failed = |source| InstallError::Download {
            url: url.to_owned(),
            source,
        };
        self.http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?
            .text()
            .await
            .map_err(failed)
    }

    /// Streams the body to `destination` and returns its SHA-256 as lowercase hex.
    async fn download(
        &self,
        url: &str,
        destination: &Path,
        progress: &InstallProgress,
    ) -> Result<String, InstallError> {
        let failed = |source| InstallError::Download {
            url: url.to_owned(),
            source,
        };
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?;
        progress.start(response.content_length());
        let mut file = tokio::fs::File::create(destination).await?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            progress.advance(chunk.len());
        }
        file.flush().await?;
        Ok(hasher.finalize().to_hex())
    }
}

/// Bytes received so far for one install's download, readable while it runs.
#[derive(Debug, Default)]
pub struct InstallProgress {
    received_bytes: AtomicU64,
    total_bytes: AtomicU64,
}

impl InstallProgress {
    pub fn snapshot(&self) -> DownloadProgress {
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        DownloadProgress {
            received_bytes: self.received_bytes.load(Ordering::Relaxed),
            total_bytes: (total_bytes > 0).then_some(total_bytes),
        }
    }

    fn start(&self, total_bytes: Option<u64>) {
        self.received_bytes.store(0, Ordering::Relaxed);
        self.total_bytes
            .store(total_bytes.unwrap_or_default(), Ordering::Relaxed);
    }

    fn advance(&self, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.received_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// How far a download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DownloadProgress {
    /// Bytes received so far.
    pub received_bytes: u64,
    /// Size of the download in bytes, absent when the server does not say.
    pub total_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Instant;

    use super::*;

    fn create_file(path: &Path) {
        fs::create_dir_all(path.parent().expect("test paths have a parent"))
            .expect("parent directory is created");
        fs::write(path, "").expect("test file is written");
    }

    #[test]
    fn claude_release_comes_from_the_manifest() {
        let manifest = format!(
            r#"{{"version":"2.1.283","platforms":{{
            "{CLAUDE_PLATFORM}":{{"binary":"claude","checksum":"ABC123","size":1}},
            "win32-x64":{{"binary":"claude","checksum":"def456","size":1}}}}}}"#
        );
        assert_eq!(
            Release::from_claude_manifest("2.1.283", &manifest).expect("manifest parses"),
            Release {
                version: "2.1.283".to_owned(),
                url: format!("{CLAUDE_RELEASES}/2.1.283/{CLAUDE_PLATFORM}/claude"),
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
    fn versions_compare_semantically() {
        assert!("2.1.10".is_newer_than("2.1.9"));
        assert!(!"2.1.9".is_newer_than("2.1.10"));
        assert!(!"2.1.9".is_newer_than("2.1.9"));
        assert!("nightly-2".is_newer_than("nightly-1"));
        assert!("nightly-1".is_newer_than("nightly-2"));
        assert!("2.1.0".is_newer_than("nightly"));
        assert!(!"nightly-1".is_newer_than("nightly-1"));
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
    fn a_kept_version_is_linked_when_the_command_is_missing() {
        let home = tempfile::tempdir().expect("temporary directory");
        let cache = tempfile::tempdir().expect("temporary directory");
        let paths =
            InstallPaths::under_home(home.path()).with_versions_in(cache.path().to_path_buf());
        assert_eq!(
            paths
                .link_kept_version(Agent::Claude)
                .expect("the cache is readable"),
            None
        );

        create_file(&cache.path().join("claude/2.1.283"));
        create_file(&cache.path().join("claude/.2.1.284.partial"));
        create_file(&cache.path().join("codex/0.150.0/bin/codex"));
        create_file(&cache.path().join("codex/0.157.1/bin/codex"));
        for (agent, version) in [(Agent::Claude, "2.1.283"), (Agent::Codex, "0.157.1")] {
            assert_eq!(
                paths
                    .link_kept_version(agent)
                    .expect("the kept version is linked")
                    .as_deref(),
                Some(version)
            );
            assert_eq!(paths.installed_version(agent).as_deref(), Some(version));
        }
    }

    #[test]
    fn the_installed_command_is_the_linked_file_and_its_version() {
        let home = tempfile::tempdir().expect("temporary directory");
        let fakes = tempfile::tempdir().expect("temporary directory");
        let paths = InstallPaths::under_home(home.path());
        assert_eq!(paths.installed_command(Agent::Codex), None);

        let claude = paths.versions_directory(Agent::Claude).join("2.1.283");
        let codex = fakes.path().join("codex/0.157.1/bin/codex");
        for (agent, target) in [(Agent::Claude, &claude), (Agent::Codex, &codex)] {
            create_file(target);
            paths
                .command(agent)
                .replace_symlink(target)
                .expect("command link is created");
        }
        assert_eq!(
            paths.installed_command(Agent::Claude),
            Some((claude, "2.1.283".to_owned()))
        );
        assert_eq!(
            paths.installed_command(Agent::Codex),
            Some((codex, "0.157.1".to_owned()))
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
        assert_eq!(paths.installed_command(Agent::Claude), None);
        assert_eq!(paths.installed_version(Agent::Claude), None);
    }

    #[test]
    fn only_the_current_version_is_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for name in ["2.1.1", "2.1.2", ".2.1.3.partial"] {
            fs::create_dir(directory.path().join(name)).expect("version directory is created");
        }
        InstallPaths::remove_versions_except(directory.path(), &["2.1.2".to_owned()])
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

    #[test]
    fn a_version_still_running_is_kept_until_it_stops() {
        let home = tempfile::tempdir().expect("temporary directory");
        let paths = InstallPaths::under_home(home.path());
        let versions = paths.versions_directory(Agent::Claude);
        fs::create_dir_all(&versions).expect("versions directory is created");
        // Ubuntu's uutils sleep exits when its file is not named after a utility.
        let shell = fs::canonicalize("/bin/sh").expect("a shell is installed");
        for version in ["2.1.1", "2.1.2", "2.1.3"] {
            let copied = Command::new("cp")
                .arg(&shell)
                .arg(versions.join(version))
                .status();
            assert!(copied.expect("cp runs").success());
        }
        paths
            .command(Agent::Claude)
            .replace_symlink(&versions.join("2.1.3"))
            .expect("command link is created");
        let mut running = Command::new(versions.join("2.1.1"))
            .arg0("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("the old version runs");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("the deadline fits");
        while Process::running_from(&versions).is_empty() {
            assert!(Instant::now() < deadline, "the old version did not start");
            thread::sleep(Duration::from_millis(20));
        }
        let remaining = || {
            let mut names: Vec<_> = versions
                .entries_or_empty()
                .expect("directory is readable")
                .into_iter()
                .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
                .collect();
            names.sort();
            names
        };

        paths
            .remove_unused_versions(Agent::Claude)
            .expect("unused versions are removed");
        assert_eq!(remaining(), ["2.1.1", "2.1.3"]);

        running.kill().expect("the old version stops");
        running.wait().expect("the old version is reaped");
        paths
            .remove_unused_versions(Agent::Claude)
            .expect("unused versions are removed");
        assert_eq!(remaining(), ["2.1.3"]);
    }
}
