use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nix::fcntl::OFlag;
use notify::EventKind;
use notify::event::{AccessKind, AccessMode};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, PersistError};
use tokio::time::sleep;
use utoipa::ToSchema;

use super::agents::{Agent, InstallPaths};
use super::events::{Events, Topic};
use super::watcher::SettledWatcher;
use crate::bytes_ext::BytesExt;
use crate::path_ext::PathExt;

/// The largest text read or saved.
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const READ_LIMIT: u64 = MAX_TEXT_BYTES as u64 + 1;
const NEW_FILE_MODE: u32 = 0o600;
const STAGING_PREFIX: &str = ".ezra.";
const RECHECK_INTERVAL: Duration = Duration::from_secs(5);
const QUIET_PERIOD: Duration = Duration::from_millis(250);
const LONGEST_SETTLE: Duration = Duration::from_secs(5);
const CODEX_WORKTREE_ROOT: &str = "/home/dev/worktrees/codex";

/// How a settings file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingsFileFormat {
    Json,
    Toml,
}

/// Where a settings file's text stops parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ParseProblem {
    pub error: String,
    /// One-based.
    pub line: usize,
    /// One-based, in characters.
    pub column: usize,
}

/// An agent's own settings file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingsFileText {
    pub path: String,
    pub format: SettingsFileFormat,
    /// Empty when the file does not exist.
    pub text: String,
    /// Identifies this text for saving.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsFileError {
    #[error("the text is larger than 2 MiB")]
    TooLarge,
    #[error("{}", .0.error)]
    Invalid(ParseProblem),
    #[error("{0} changed since it was opened")]
    Changed(String),
    #[error("{0} is not UTF-8 text")]
    NotText(String),
    #[error("{0} is not a regular file")]
    NotRegular(String),
    #[error("{0} is larger than 2 MiB")]
    FileTooLarge(String),
    #[error("could not read or write {path}: {source}")]
    Io { path: String, source: io::Error },
}

impl SettingsFileFormat {
    /// Where `text` stops parsing the way the agent reads it, or `None` when it parses.
    pub fn problem(self, text: &str) -> Option<ParseProblem> {
        match self {
            Self::Json => {
                if text.chars().all(|character| {
                    character == '\u{feff}' || (character.is_whitespace() && character != '\u{85}')
                }) {
                    return None;
                }
                let json = text.strip_prefix('\u{feff}').unwrap_or(text);
                let rejected = serde_json::from_str::<IgnoredAny>(json).err()?;
                let described = serde_json::from_str::<serde_json::Value>(json)
                    .err()
                    .filter(|described| {
                        (described.line(), described.column())
                            >= (rejected.line(), rejected.column())
                    })
                    .unwrap_or(rejected);
                let line_start: usize = json
                    .split_inclusive('\n')
                    .take(described.line().saturating_sub(1))
                    .map(str::len)
                    .sum();
                let offset = text.len().saturating_sub(json.len()).saturating_add(
                    line_start
                        .saturating_add(described.column())
                        .saturating_sub(1),
                );
                let message = described.to_string();
                let position = format!(
                    " at line {} column {}",
                    described.line(),
                    described.column()
                );
                Some(ParseProblem::at(
                    text,
                    offset,
                    message.strip_suffix(&position).unwrap_or(&message),
                ))
            }
            Self::Toml => {
                let error = toml::from_str::<toml::Table>(text).err()?;
                Some(ParseProblem::at(
                    text,
                    error.span().map_or(0, |span| span.start),
                    error.message(),
                ))
            }
        }
    }
}

impl ParseProblem {
    /// `error` at the character that starts at, or contains, byte `offset` of `text`.
    fn at(text: &str, offset: usize, error: &str) -> Self {
        let before = text.get(..text.floor_char_boundary(offset)).unwrap_or(text);
        let line_start = before
            .rfind('\n')
            .map_or(0, |newline| newline.saturating_add(1));
        Self {
            error: error.to_owned(),
            line: before.matches('\n').count().saturating_add(1),
            column: before
                .get(line_start..)
                .unwrap_or_default()
                .chars()
                .count()
                .saturating_add(1),
        }
    }
}

/// An agent CLI's own settings file, saved as the exact text sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFile {
    pub path: PathBuf,
    pub format: SettingsFileFormat,
}

impl InstallPaths {
    /// Sets Codex's worktree root only when its user settings leave it unset. Blocks.
    pub fn set_default_codex_worktree_root(&self) -> Result<(), SettingsFileError> {
        let Some(file) = self.settings_file(Agent::Codex) else {
            return Ok(());
        };
        let current = file.read()?;
        let mut document = current
            .text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| {
                file.failed(io::Error::new(
                    io::ErrorKind::InvalidData,
                    error.message().to_owned(),
                ))
            })?;
        let desktop = document
            .entry("desktop")
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_like_mut()
            .ok_or_else(|| {
                file.failed(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "desktop is not a TOML table",
                ))
            })?;
        if desktop.contains_key("git-worktree-root") {
            return Ok(());
        }
        desktop.insert("git-worktree-root", toml_edit::value(CODEX_WORKTREE_ROOT));
        file.save(document.to_string(), &current.version)?;
        Ok(())
    }

    /// Claude Code's `settings.json` or Codex's `config.toml`, absent when the agent's config
    /// directory is unknown.
    pub fn settings_file(&self, agent: Agent) -> Option<SettingsFile> {
        let (name, format) = match agent {
            Agent::Claude => ("settings.json", SettingsFileFormat::Json),
            Agent::Codex => ("config.toml", SettingsFileFormat::Toml),
        };
        Some(SettingsFile {
            path: self.config_directory(agent)?.join(name),
            format,
        })
    }
}

impl SettingsFile {
    /// The file's text, empty when it does not exist. Blocks.
    pub fn read(&self) -> Result<SettingsFileText, SettingsFileError> {
        let text = String::from_utf8(self.bytes()?)
            .map_err(|_| SettingsFileError::NotText(self.path.display().to_string()))?;
        Ok(self.with_text(text))
    }

    /// Writes `text` byte for byte when it parses and the file is still at `version`. Blocks.
    pub fn save(&self, text: String, version: &str) -> Result<SettingsFileText, SettingsFileError> {
        static SAVING: Mutex<()> = Mutex::new(());
        if text.len() > MAX_TEXT_BYTES {
            return Err(SettingsFileError::TooLarge);
        }
        if let Some(problem) = self.format.problem(&text) {
            return Err(SettingsFileError::Invalid(problem));
        }
        let staged = self
            .stage(text.as_bytes())
            .map_err(|source| self.failed(source))?;
        let _saving = SAVING.lock().unwrap_or_else(PoisonError::into_inner);
        let current = self.bytes()?;
        if Self::version_of(&current) != version {
            return Err(SettingsFileError::Changed(self.path.display().to_string()));
        }
        staged
            .replace_target(text.as_bytes())
            .map_err(|source| self.failed(source))?;
        Ok(self.with_text(text))
    }

    fn with_text(&self, text: String) -> SettingsFileText {
        SettingsFileText {
            path: self.path.display().to_string(),
            format: self.format,
            version: Self::version_of(text.as_bytes()),
            text,
        }
    }

    fn failed(&self, source: io::Error) -> SettingsFileError {
        SettingsFileError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn version_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes).to_hex()
    }

    /// The file's bytes, none when it does not exist. Refuses anything but a regular file of at
    /// most 2 MiB without waiting on it. Blocks.
    fn bytes(&self) -> Result<Vec<u8>, SettingsFileError> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(OFlag::O_NONBLOCK.bits())
            .open(&self.path)
        {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            file => file.map_err(|source| self.failed(source))?,
        };
        if !file
            .metadata()
            .map_err(|source| self.failed(source))?
            .is_file()
        {
            return Err(SettingsFileError::NotRegular(
                self.path.display().to_string(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(READ_LIMIT)
            .read_to_end(&mut bytes)
            .map_err(|source| self.failed(source))?;
        if bytes.len() > MAX_TEXT_BYTES {
            return Err(SettingsFileError::FileTooLarge(
                self.path.display().to_string(),
            ));
        }
        Ok(bytes)
    }

    /// Writes `bytes` to a new file beside the file the path's links lead to, with that file's
    /// mode. Blocks.
    fn stage(&self, bytes: &[u8]) -> io::Result<StagedFile> {
        let target = self.path.link_target()?;
        let directory = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(directory)?;
        let mode = fs::metadata(&target).map_or(NEW_FILE_MODE, |metadata| {
            metadata.permissions().mode() & 0o7777
        });
        let mut file = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(directory)?;
        file.as_file()
            .set_permissions(Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        Ok(StagedFile { file, target })
    }

    /// This file, the file its links lead to and its directory. Blocks.
    fn watched_paths(&self) -> impl Iterator<Item = PathBuf> {
        [
            Some(self.path.clone()),
            self.path.link_target().ok(),
            self.path.parent().map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten()
    }
}

/// New text for a settings file, written beside the file it replaces and removed if dropped.
struct StagedFile {
    file: NamedTempFile,
    target: PathBuf,
}

impl StagedFile {
    /// Moves the new text into place. A file that cannot be replaced, such as one mounted on its
    /// own, gets `bytes` written over it instead. Blocks.
    fn replace_target(self, bytes: &[u8]) -> io::Result<()> {
        let Err(PersistError { error, file }) = self.file.persist(&self.target) else {
            return Ok(());
        };
        if !matches!(
            error.kind(),
            io::ErrorKind::ResourceBusy
                | io::ErrorKind::CrossesDevices
                | io::ErrorKind::PermissionDenied
        ) {
            return Err(error);
        }
        drop(file);
        let mut target = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.target)?;
        target.write_all(bytes)?;
        target.sync_all()
    }
}

/// Notices when an agent's settings file changes on disk, whoever writes it. Watches the
/// directories that hold the files and their link targets, and also rereads the files every 5 s.
pub struct SettingsFileWatcher {
    files: Vec<SettingsFile>,
    versions: Vec<String>,
    watcher: SettledWatcher,
    relevant: Arc<RelevantPaths>,
}

/// The paths whose changes matter, shared with the watcher's callback.
#[derive(Default)]
struct RelevantPaths(Mutex<BTreeSet<PathBuf>>);

impl RelevantPaths {
    /// True when `event` wrote, made, removed or renamed one of the paths.
    fn changed_by(&self, event: &notify::Event) -> bool {
        let read = matches!(
            event.kind,
            EventKind::Access(kind) if kind != AccessKind::Close(AccessMode::Write)
        );
        !read
            && self.0.lock().map_or(true, |paths| {
                event.paths.iter().any(|path| paths.contains(path))
            })
    }
}

impl SettingsFileWatcher {
    /// Reads each agent's settings file and starts watching it.
    pub async fn start(paths: &InstallPaths) -> Self {
        let relevant = Arc::new(RelevantPaths::default());
        let filter = Arc::clone(&relevant);
        let watcher = SettledWatcher::start(&[], move |event| filter.changed_by(event))
            .inspect_err(|error| {
                tracing::warn!(
                    "cannot watch the agents' settings files, rereading them every 5 s instead: {error}"
                );
            })
            .unwrap_or_default();
        let mut started = Self {
            files: Agent::ALL
                .into_iter()
                .filter_map(|agent| paths.settings_file(agent))
                .collect(),
            versions: Vec::new(),
            watcher,
            relevant,
        };
        started.look().await;
        started
    }

    /// Publishes `SettingsFile` after each change.
    pub async fn publish_changes(mut self, events: Events) {
        loop {
            self.next_change().await;
            events.publish(Topic::SettingsFile);
        }
    }

    /// Returns once a file's content differs from when it was last read.
    async fn next_change(&mut self) {
        loop {
            tokio::select! {
                () = sleep(RECHECK_INTERVAL) => {}
                () = self.watcher.settled_change(QUIET_PERIOD, LONGEST_SETTLE) => {}
            }
            if self.look().await {
                return;
            }
        }
    }

    /// Watches where the files are now and rereads them. True when one changed.
    async fn look(&mut self) -> bool {
        let files = self.files.clone();
        let Ok(paths) = tokio::task::spawn_blocking(move || {
            files
                .iter()
                .flat_map(SettingsFile::watched_paths)
                .collect::<BTreeSet<PathBuf>>()
        })
        .await
        else {
            return false;
        };
        self.watcher.watch_only(
            paths
                .iter()
                .filter_map(|path| path.parent())
                .map(Path::to_path_buf)
                .collect(),
        );
        if let Ok(mut relevant) = self.relevant.0.lock() {
            *relevant = paths;
        }
        let files = self.files.clone();
        let Ok(versions) = tokio::task::spawn_blocking(move || {
            files
                .iter()
                .map(|file| {
                    file.bytes().map_or_else(
                        |error| error.to_string(),
                        |bytes| SettingsFile::version_of(&bytes),
                    )
                })
                .collect::<Vec<String>>()
        })
        .await
        else {
            return false;
        };
        let changed = self.versions != versions;
        self.versions = versions;
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::fs::{FileTypeExt, symlink};
    use std::sync::{Barrier, mpsc};
    use std::thread;

    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    use tokio::time::timeout;

    use super::*;

    const EMPTY_VERSION: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn problem(format: SettingsFileFormat, text: &str) -> Option<(usize, usize, String)> {
        format
            .problem(text)
            .map(|problem| (problem.line, problem.column, problem.error))
    }

    fn at(line: usize, column: usize, error: &str) -> Option<(usize, usize, String)> {
        Some((line, column, error.to_owned()))
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("the file exists")
            .permissions()
            .mode()
            & 0o7777
    }

    fn promptly<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || sender.send(work()));
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the work finishes within 2 s")
    }

    fn sparse_file(path: &Path, length: u64) {
        File::create(path)
            .and_then(|file| file.set_len(length))
            .expect("the sparse file is created");
    }

    struct Files {
        directory: tempfile::TempDir,
        paths: InstallPaths,
    }

    impl Files {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary directory");
            let paths = InstallPaths::under_home(&directory.path().join("home"))
                .with_config_directories(
                    directory.path().join("config/claude"),
                    directory.path().join("config/codex"),
                );
            Self { directory, paths }
        }

        fn of(&self, agent: Agent) -> SettingsFile {
            self.paths
                .settings_file(agent)
                .expect("the config directory is known")
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.directory.path().join(relative)
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().expect("the path has a parent"))
                .expect("the directory is created");
            fs::write(path, bytes).expect("the file is written");
        }

        fn read(&self, relative: &str) -> Vec<u8> {
            fs::read(self.path(relative)).expect("the file is read")
        }

        fn names_in(&self, relative: &str) -> Vec<String> {
            let mut names: Vec<String> = self
                .path(relative)
                .entries_or_empty()
                .expect("the directory is listed")
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    #[test]
    fn json_is_checked_the_way_claude_reads_it() {
        use SettingsFileFormat::Json;
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        for valid in [
            "",
            " \n\t\r\n",
            "\u{feff}",
            "\u{feff}\u{a0}\u{2028}\u{3000} ",
            "\u{feff}{}",
            "{}",
            "[]",
            "1",
            "null",
            r#"{"unknown":{"keys":[1,"two",null]},"a":"\ud800","b":1e400}"#,
            "{\"a\":\"x\u{2028}y\"}",
            "{\r\n\t\"a\" :  1\r\n}\r\n\r\n",
            &deep,
        ] {
            assert_eq!(problem(Json, valid), None, "{valid:?}");
        }
        assert_eq!(problem(Json, r#"{"a":1,}"#), at(1, 8, "trailing comma"));
        assert_eq!(
            problem(Json, "{\r\n  \"a\": 1,\r\n}\r\n"),
            at(3, 1, "trailing comma")
        );
        assert_eq!(
            problem(Json, "\u{feff}{\"a\":1,}"),
            at(1, 9, "trailing comma")
        );
        assert_eq!(
            problem(Json, "{\"\u{e9}\u{e9}\u{e9}\":1,}"),
            at(1, 10, "trailing comma")
        );
        assert_eq!(
            problem(Json, "{\n\"\u{1f600}\": 1,\n\"b\": x}"),
            at(3, 6, "expected value")
        );
        assert_eq!(problem(Json, "// c\n{}"), at(1, 1, "expected value"));
        assert_eq!(problem(Json, "{/* c */}"), at(1, 2, "key must be a string"));
        assert_eq!(problem(Json, "{\"a\":01}"), at(1, 7, "invalid number"));
        assert_eq!(problem(Json, "{'a':1}"), at(1, 2, "key must be a string"));
        assert_eq!(problem(Json, "{} x"), at(1, 4, "trailing characters"));
        assert_eq!(
            problem(Json, "{\"a\":\"x\ty\"}"),
            at(
                1,
                8,
                "control character (\\u0000-\\u001F) found while parsing a string"
            )
        );
        assert_eq!(problem(Json, "{\"a\":NaN}"), at(1, 6, "expected value"));
        assert_eq!(problem(Json, "\u{a0}{}"), at(1, 1, "expected value"));
        assert_eq!(
            problem(Json, "\u{feff}\u{feff}{}"),
            at(1, 2, "expected value")
        );
        assert_eq!(problem(Json, "\u{85}"), at(1, 1, "expected value"));
        assert_eq!(
            problem(Json, "{\n"),
            at(1, 2, "EOF while parsing an object")
        );
        for unterminated in [
            "{\n  \"model\": \"opus\n}\n",
            "{\r\n  \"model\": \"opus\r\n}\r\n",
        ] {
            assert_eq!(
                problem(Json, unterminated),
                at(
                    2,
                    17,
                    "control character (\\u0000-\\u001F) found while parsing a string"
                ),
                "{unterminated:?}"
            );
        }
        for cut_short in [
            "{\"a\":\"cafe",
            "{\"a\":\"caf\u{e9}",
            "{\"a\":\"caf\u{1f600}",
        ] {
            assert_eq!(
                problem(Json, cut_short),
                at(1, 10, "EOF while parsing a string"),
                "{cut_short:?}"
            );
        }
        assert_eq!(
            problem(Json, r#"{"a":"\ud800","b":1,}"#),
            at(1, 21, "key must be a string")
        );
        assert_eq!(
            problem(Json, &format!("{}1,{}", "[".repeat(200), "]".repeat(200))),
            at(1, 203, "expected value")
        );
        for (text, line, column, error) in [
            ("{\"a\": ture}", 1, 8, "expected ident"),
            ("{\"a\": True}", 1, 7, "expected value"),
            ("{\"a\": nul}", 1, 10, "expected ident"),
            ("{\"a\":tru}", 1, 9, "expected ident"),
            ("{\"a\": faase}", 1, 9, "expected ident"),
            ("{\"a\": nan}", 1, 8, "expected ident"),
            ("{\"a\": [tru]}", 1, 11, "expected ident"),
            (
                "{\n  \"a\": true,\n  \"b\": fasle\n}\n",
                3,
                10,
                "expected ident",
            ),
            ("{\"a\": undefined}", 1, 7, "expected value"),
            ("{\"a\": 'x'}", 1, 7, "expected value"),
            ("{\"a\": .5}", 1, 7, "expected value"),
            ("{\"a\": +1}", 1, 7, "expected value"),
            ("{\"a\": 1 nul}", 1, 9, "expected `,` or `}`"),
            ("{\"a\": 1, nul}", 1, 10, "key must be a string"),
            ("{", 1, 1, "EOF while parsing an object"),
            ("[", 1, 1, "EOF while parsing a list"),
            ("{\"a\":1\n\n", 2, 1, "EOF while parsing an object"),
            ("{\"a\":1,", 1, 7, "EOF while parsing a value"),
            ("{\"a\":\"b\"", 1, 8, "EOF while parsing an object"),
            ("{\"a\": tr", 1, 8, "EOF while parsing a value"),
            ("tru", 1, 3, "EOF while parsing a value"),
            ("[1,]", 1, 4, "trailing comma"),
            ("{\"a\":[1,\r\n],\"b\":2}", 2, 1, "trailing comma"),
            ("{\"a\":1 , }", 1, 10, "trailing comma"),
            ("{\"a\":1,,\"b\":2}", 1, 8, "key must be a string"),
            ("\u{feff}{} x", 1, 5, "trailing characters"),
            ("{}}", 1, 3, "trailing characters"),
            ("{\"a\":1}\n{\"b\":2}\n", 2, 1, "trailing characters"),
        ] {
            assert_eq!(problem(Json, text), at(line, column, error), "{text:?}");
        }
    }

    #[test]
    fn toml_is_checked_as_toml_1_1() {
        use SettingsFileFormat::Toml;
        for valid in [
            "",
            "  \n\n",
            "\u{feff}model = \"gpt\"\r\n[features]\r\n\tx = true\r\n",
            "a = {\n  b = 1,\n  c = 2,\n}\ns = \"\\e\\x41\"\nt = 07:32\nd = 1979-05-27T07:32Z\n",
            "[projects.\"/home/dev/projects/a\"]\ntrust_level = \"trusted\"",
            "t = 07:32:60\n",
            "d = 1979-05-27T23:59:60Z\n",
            "t = 07:32:60.5\nd = 1979-12-31T23:59:60+01:00\n",
            "a = 9223372036854775807\nb = -9223372036854775808\nc = 9007199254740992\n",
            "a = 0x7FFFFFFFFFFFFFFF\n",
        ] {
            assert_eq!(problem(Toml, valid), None, "{valid:?}");
        }
        assert_eq!(
            problem(Toml, "a = 9223372036854775808\n"),
            at(1, 5, "u64 value was too large")
        );
        assert_eq!(
            problem(Toml, "a = -9223372036854775809\n"),
            at(
                1,
                5,
                "invalid type: integer `-9223372036854775809` as i128, expected any valid TOML value"
            )
        );
        assert_eq!(problem(Toml, "a = 1\na = 2\n"), at(2, 1, "duplicate key"));
        assert_eq!(problem(Toml, "[a]\n[a]\n"), at(2, 2, "duplicate key"));
        assert_eq!(
            problem(Toml, "\u{feff}a = 1\r\na = 2\r\n"),
            at(2, 1, "duplicate key")
        );
        assert_eq!(
            problem(Toml, "a = \"\u{e9}\u{1f600}\" b = 1\n"),
            at(1, 10, "unexpected key or value, expected newline, `#`")
        );
        for (text, line, column, error) in [
            ("a = {b = 1, b = 2}\n", 1, 13, "duplicate key"),
            (
                "d = 1979-02-30\n",
                1,
                5,
                "invalid date, expected day between 01 and 28",
            ),
            (
                "d = 1900-02-29\n",
                1,
                5,
                "invalid date, expected day between 01 and 28",
            ),
            (
                "d = 1979-04-31\n",
                1,
                5,
                "invalid date, expected day between 01 and 30",
            ),
            (
                "a = 1\rb = 2\n",
                1,
                7,
                "carriage return must be followed by newline, expected newline",
            ),
            ("[a]\nb.c = 1\n[a.b]\nd = 2\n", 3, 4, "duplicate key"),
            (
                "[projects.\"/p\"]\ntrust_level = \"a\"\n[projects.\"/p\"]\ntrust_level = \"b\"\n",
                3,
                11,
                "duplicate key",
            ),
        ] {
            assert_eq!(problem(Toml, text), at(line, column, error), "{text:?}");
        }
    }

    #[test]
    fn codex_worktree_default_creates_private_settings_and_is_idempotent() {
        let files = Files::new();
        files
            .paths
            .set_default_codex_worktree_root()
            .expect("the default is written");
        let codex = files.of(Agent::Codex);
        let first = codex.read().expect("the file reads");
        let document: toml::Value = toml::from_str(&first.text).expect("the settings parse");
        assert_eq!(
            document["desktop"]["git-worktree-root"].as_str(),
            Some(CODEX_WORKTREE_ROOT)
        );
        assert_eq!(mode_of(&codex.path), 0o600);
        let modified = fs::metadata(&codex.path)
            .expect("the file exists")
            .modified()
            .expect("the modification time exists");
        files
            .paths
            .set_default_codex_worktree_root()
            .expect("the default is already set");
        assert_eq!(codex.read().expect("the file reads"), first);
        assert_eq!(
            fs::metadata(&codex.path)
                .expect("the file exists")
                .modified()
                .expect("the modification time exists"),
            modified
        );
        assert!(!files.of(Agent::Claude).path.exists());
    }

    #[test]
    fn codex_worktree_default_preserves_every_existing_value() {
        let files = Files::new();
        for text in [
            "# my choice\n[desktop]\ngit-worktree-root = '/mnt/worktrees' # keep\n",
            "\u{feff}desktop.git-worktree-root = \"\"\r\n",
            "desktop = { git-worktree-root = false }\n",
            "[desktop.git-worktree-root]\ncustom = true\n",
        ] {
            files.write("config/codex/config.toml", text.as_bytes());
            files
                .paths
                .set_default_codex_worktree_root()
                .expect("an existing value is kept");
            assert_eq!(files.read("config/codex/config.toml"), text.as_bytes());
        }
    }

    #[test]
    fn codex_worktree_default_preserves_other_settings_and_comments() {
        let files = Files::new();
        for text in [
            "# my model\nmodel = 'mine' # keep\n",
            "# desktop settings\n[desktop]\nother = true # keep\n",
            "desktop = { other = true } # keep\n",
            "desktop.other = true # keep\n",
        ] {
            files.write("config/codex/config.toml", text.as_bytes());
            let original: toml::Value = toml::from_str(text).expect("the original settings parse");
            files
                .paths
                .set_default_codex_worktree_root()
                .expect("the missing key is added");
            let updated = files.of(Agent::Codex).read().expect("the file reads").text;
            assert!(updated.contains("# keep"), "{updated}");
            let mut document: toml::Value =
                toml::from_str(&updated).expect("the updated settings parse");
            let desktop = document["desktop"]
                .as_table_mut()
                .expect("desktop is a table");
            assert_eq!(
                desktop.remove("git-worktree-root"),
                Some(CODEX_WORKTREE_ROOT.into())
            );
            if !original
                .as_table()
                .expect("the document is a table")
                .contains_key("desktop")
            {
                document
                    .as_table_mut()
                    .expect("the document is a table")
                    .remove("desktop");
            }
            assert_eq!(document, original);
        }
    }

    #[test]
    fn codex_worktree_default_leaves_invalid_documents_untouched() {
        let files = Files::new();
        for text in [
            "model = [",
            "desktop = false\n",
            "[[desktop]]\nother = true\n",
        ] {
            files.write("config/codex/config.toml", text.as_bytes());
            assert!(files.paths.set_default_codex_worktree_root().is_err());
            assert_eq!(files.read("config/codex/config.toml"), text.as_bytes());
        }
    }

    #[test]
    fn a_missing_file_reads_empty_and_saving_creates_it() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        let missing = claude.read().expect("a missing file reads");
        assert_eq!(
            missing,
            SettingsFileText {
                path: files
                    .path("config/claude/settings.json")
                    .display()
                    .to_string(),
                format: SettingsFileFormat::Json,
                text: String::new(),
                version: EMPTY_VERSION.to_owned(),
            }
        );

        let saved = claude
            .save("{}".to_owned(), &missing.version)
            .expect("the file is created");
        assert_eq!(files.read("config/claude/settings.json"), b"{}");
        assert_eq!(mode_of(&claude.path), 0o600);
        assert_eq!(claude.read().expect("the file reads"), saved);
        assert_eq!(files.names_in("config/claude"), ["settings.json"]);

        let codex = files.of(Agent::Codex);
        assert_eq!(codex.format, SettingsFileFormat::Toml);
        assert!(codex.path.ends_with("config/codex/config.toml"));
    }

    #[test]
    fn text_is_saved_byte_for_byte_and_the_mode_is_kept() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        files.write("config/claude/settings.json", b"{}");
        fs::set_permissions(&claude.path, Permissions::from_mode(0o660)).expect("the mode is set");
        let mut version = claude.read().expect("the file reads").version;
        for text in [
            "\u{feff}{\r\n\t\"a\" :   1 ,\r\n  \"b\":[ ]\r\n}",
            "{\"env\": {\"A\": \"\u{e9}\"}}\n\n\n",
            "\u{feff}",
            "",
        ] {
            let saved = claude
                .save(text.to_owned(), &version)
                .expect("the text saves");
            assert_eq!(files.read("config/claude/settings.json"), text.as_bytes());
            assert_eq!(saved.text, text);
            assert_eq!(mode_of(&claude.path), 0o660);
            version = saved.version;
        }

        let codex = files.of(Agent::Codex);
        let text = "\u{feff}# mine\r\nmodel = \"gpt\"   # trailing\r\n\r\n[features]\r\n\tx = true";
        codex
            .save(text.to_owned(), EMPTY_VERSION)
            .expect("the TOML saves");
        assert_eq!(files.read("config/codex/config.toml"), text.as_bytes());
    }

    #[test]
    fn a_stale_version_is_refused() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        files.write("config/claude/settings.json", b"{\"a\":1}");
        let opened = claude.read().expect("the file reads");

        files.write("config/claude/settings.json", b"{\"a\":2}");
        assert!(matches!(
            claude.save("{\"a\":3}".to_owned(), &opened.version),
            Err(SettingsFileError::Changed(_))
        ));
        assert_eq!(files.read("config/claude/settings.json"), b"{\"a\":2}");
        assert_eq!(files.names_in("config/claude"), ["settings.json"]);

        let opened = claude.read().expect("the file reads");
        fs::remove_file(&claude.path).expect("the file is removed");
        assert!(matches!(
            claude.save("{\"a\":3}".to_owned(), &opened.version),
            Err(SettingsFileError::Changed(_))
        ));
        assert!(!claude.path.exists());

        let codex = files.of(Agent::Codex);
        let opened = codex.read().expect("a missing file reads");
        files.write("config/codex/config.toml", b"model = \"o3\"\n");
        assert!(matches!(
            codex.save("model = \"gpt\"\n".to_owned(), &opened.version),
            Err(SettingsFileError::Changed(_))
        ));
        assert_eq!(files.read("config/codex/config.toml"), b"model = \"o3\"\n");
    }

    #[test]
    fn saves_from_the_same_version_do_not_both_win() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        let tabs = 4;
        for round in 0..10 {
            let opened = claude.read().expect("the file reads");
            let start = Barrier::new(tabs);
            let outcomes: Vec<_> = thread::scope(|scope| {
                let saves: Vec<_> = (0..tabs)
                    .map(|tab| {
                        let (claude, start, opened) = (&claude, &start, &opened);
                        let text = format!(
                            "{{\"round\": {round}, \"tab\": {tab}, \"padding\": \"{}\"}}\n",
                            "x".repeat(tab.saturating_mul(4096))
                        );
                        scope.spawn(move || {
                            start.wait();
                            claude.save(text, &opened.version)
                        })
                    })
                    .collect();
                saves
                    .into_iter()
                    .map(|save| save.join().expect("the save finishes"))
                    .collect()
            });
            let saved: Vec<&SettingsFileText> = outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().ok())
                .collect();
            let [saved] = saved.as_slice() else {
                panic!("{} saves won round {round}", saved.len());
            };
            assert!(
                outcomes
                    .iter()
                    .all(|outcome| matches!(outcome, Ok(_) | Err(SettingsFileError::Changed(_)))),
                "{outcomes:?}"
            );
            assert_eq!(
                files.read("config/claude/settings.json"),
                saved.text.as_bytes()
            );
            assert_eq!(files.names_in("config/claude"), ["settings.json"]);
        }
    }

    #[test]
    fn text_that_does_not_parse_or_is_too_large_is_refused() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        let Err(SettingsFileError::Invalid(problem)) =
            claude.save("{\n  \"a\": 1,\n}\n".to_owned(), EMPTY_VERSION)
        else {
            panic!("a trailing comma was saved");
        };
        assert_eq!((problem.line, problem.column), (3, 1));
        assert!(!claude.path.exists());

        let largest = format!("\"{}\"", "a".repeat(MAX_TEXT_BYTES.saturating_sub(2)));
        let saved = claude
            .save(largest.clone(), EMPTY_VERSION)
            .expect("2 MiB saves");
        assert!(matches!(
            claude.save(format!("{largest} "), &saved.version),
            Err(SettingsFileError::TooLarge)
        ));

        let codex = files.of(Agent::Codex);
        assert!(matches!(
            codex.save("a = 1\na = 2\n".to_owned(), EMPTY_VERSION),
            Err(SettingsFileError::Invalid(_))
        ));
        assert!(!codex.path.exists());
    }

    #[test]
    fn a_linked_file_is_written_through_the_link() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        files.write("dotfiles/claude.json", b"{}");
        fs::set_permissions(
            files.path("dotfiles/claude.json"),
            Permissions::from_mode(0o664),
        )
        .expect("the mode is set");
        fs::create_dir_all(files.path("config/claude")).expect("the directory is created");
        symlink("../../dotfiles/claude.json", &claude.path).expect("the link is created");

        let opened = claude.read().expect("the file reads through the link");
        assert_eq!(opened.text, "{}");
        claude
            .save("{\"a\": 1}\n".to_owned(), &opened.version)
            .expect("the file saves");
        assert_eq!(
            fs::read_link(&claude.path).expect("the link stays"),
            Path::new("../../dotfiles/claude.json")
        );
        assert_eq!(files.read("dotfiles/claude.json"), b"{\"a\": 1}\n");
        assert_eq!(mode_of(&files.path("dotfiles/claude.json")), 0o664);

        let codex = files.of(Agent::Codex);
        fs::create_dir_all(files.path("config/codex")).expect("the directory is created");
        symlink(files.path("elsewhere/config.toml"), &codex.path).expect("the link is created");
        codex
            .save("model = \"gpt\"\n".to_owned(), EMPTY_VERSION)
            .expect("the missing target is created");
        assert!(
            fs::symlink_metadata(&codex.path)
                .expect("the link stays")
                .is_symlink()
        );
        assert_eq!(files.read("elsewhere/config.toml"), b"model = \"gpt\"\n");
        assert_eq!(mode_of(&files.path("elsewhere/config.toml")), 0o600);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused() {
        let files = Files::new();
        files.write("config/claude/settings.json", b"\xff\xfe{\x00}\x00");
        assert!(matches!(
            files.of(Agent::Claude).read(),
            Err(SettingsFileError::NotText(_))
        ));
    }

    #[test]
    fn only_a_regular_file_of_at_most_2_mib_is_read() {
        let files = Files::new();
        let claude = files.of(Agent::Claude);
        fs::create_dir_all(files.path("config/claude")).expect("the directory is created");
        mkfifo(&claude.path, Mode::S_IRUSR | Mode::S_IWUSR).expect("the pipe is created");
        let pipe = claude.clone();
        assert!(matches!(
            promptly(move || pipe.read()),
            Err(SettingsFileError::NotRegular(_))
        ));
        let pipe = claude.clone();
        assert!(matches!(
            promptly(move || pipe.save("{}".to_owned(), EMPTY_VERSION)),
            Err(SettingsFileError::NotRegular(_))
        ));
        assert!(
            fs::symlink_metadata(&claude.path)
                .expect("the pipe stays")
                .file_type()
                .is_fifo()
        );
        assert_eq!(files.names_in("config/claude"), ["settings.json"]);

        fs::remove_file(&claude.path).expect("the pipe is removed");
        symlink("/dev/zero", &claude.path).expect("the link is created");
        let endless = claude.clone();
        assert!(matches!(
            promptly(move || endless.read()),
            Err(SettingsFileError::NotRegular(_))
        ));

        fs::remove_file(&claude.path).expect("the link is removed");
        fs::create_dir(&claude.path).expect("the directory is created");
        assert!(matches!(
            claude.read(),
            Err(SettingsFileError::NotRegular(_))
        ));

        fs::remove_dir(&claude.path).expect("the directory is removed");
        sparse_file(&claude.path, MAX_TEXT_BYTES as u64);
        assert_eq!(
            claude.read().expect("2 MiB reads").text.len(),
            MAX_TEXT_BYTES
        );
        sparse_file(&claude.path, 4 * 1024 * 1024 * 1024);
        let huge = claude.clone();
        assert!(matches!(
            promptly(move || huge.read()),
            Err(SettingsFileError::FileTooLarge(_))
        ));
        let huge = claude.clone();
        assert!(matches!(
            promptly(move || huge.save("{}".to_owned(), EMPTY_VERSION)),
            Err(SettingsFileError::FileTooLarge(_))
        ));
    }

    async fn noticed(watcher: &mut SettingsFileWatcher, what: &str) {
        timeout(Duration::from_secs(4), watcher.next_change())
            .await
            .unwrap_or_else(|_| panic!("{what} was not noticed"));
    }

    async fn not_noticed(watcher: &mut SettingsFileWatcher, what: &str) {
        assert!(
            timeout(Duration::from_millis(1500), watcher.next_change())
                .await
                .is_err(),
            "{what} was noticed"
        );
    }

    #[tokio::test]
    async fn changes_by_the_agents_are_noticed() {
        let files = Files::new();
        fs::create_dir(files.path("config")).expect("the config volume is created");
        let mut watcher = SettingsFileWatcher::start(&files.paths).await;

        files.write("config/claude/settings.json", b"{}");
        noticed(&mut watcher, "a new directory and file").await;

        files.write("config/claude/.cc-writes/.tmp.12.ab", b"{\"a\":1}");
        fs::rename(
            files.path("config/claude/.cc-writes/.tmp.12.ab"),
            files.path("config/claude/settings.json"),
        )
        .expect("the file is replaced");
        noticed(&mut watcher, "Claude's own save").await;

        files.write(
            "config/codex/.tmpAbC123",
            b"[projects.\"/home/dev/projects/a\"]\n",
        );
        fs::rename(
            files.path("config/codex/.tmpAbC123"),
            files.path("config/codex/config.toml"),
        )
        .expect("the file is replaced");
        noticed(&mut watcher, "Codex's own save").await;

        let mut file = OpenOptions::new()
            .append(true)
            .open(files.path("config/codex/config.toml"))
            .expect("the file opens");
        file.write_all(b"trust_level = \"trusted\"\n")
            .expect("the file is appended to");
        drop(file);
        noticed(&mut watcher, "a write in place").await;

        fs::remove_file(files.path("config/codex/config.toml")).expect("the file is removed");
        noticed(&mut watcher, "a removal").await;

        files.write("config/claude/.claude.json", b"{}");
        files.write("config/claude/settings.json.ezra", b"{\"b\":2}");
        files.write("config/codex/history.jsonl", b"{}\n");
        fs::read(files.path("config/claude/settings.json")).expect("the file reads");
        let settings = files.path("config/claude/settings.json");
        fs::set_permissions(&settings, Permissions::from_mode(0o600)).expect("the mode is set");
        not_noticed(&mut watcher, "a change to other files").await;
    }

    #[tokio::test]
    async fn files_that_are_not_regular_or_too_large_do_not_stall_the_watcher() {
        let files = Files::new();
        let claude = files.path("config/claude/settings.json");
        let codex = files.path("config/codex/config.toml");
        fs::create_dir_all(files.path("config/claude")).expect("the directory is created");
        fs::create_dir_all(files.path("config/codex")).expect("the directory is created");
        mkfifo(&claude, Mode::S_IRUSR | Mode::S_IWUSR).expect("the pipe is created");
        symlink("/dev/zero", &codex).expect("the link is created");
        let mut watcher = timeout(
            Duration::from_secs(2),
            SettingsFileWatcher::start(&files.paths),
        )
        .await
        .expect("the watcher starts");

        fs::remove_file(&claude).expect("the pipe is removed");
        files.write("config/claude/settings.json", b"{}");
        noticed(&mut watcher, "a file in place of a pipe").await;

        fs::remove_file(&codex).expect("the link is removed");
        sparse_file(&codex, READ_LIMIT);
        noticed(&mut watcher, "a file over 2 MiB in place of a link").await;

        files.write("config/claude/settings.json", b"{\"a\":1}");
        noticed(&mut watcher, "a change beside a file over 2 MiB").await;
    }

    #[tokio::test]
    async fn changes_to_a_link_target_are_noticed() {
        let files = Files::new();
        files.write("dotfiles/claude.json", b"{}");
        fs::create_dir_all(files.path("config/claude")).expect("the directory is created");
        symlink(
            files.path("dotfiles/claude.json"),
            files.path("config/claude/settings.json"),
        )
        .expect("the link is created");
        let mut watcher = SettingsFileWatcher::start(&files.paths).await;

        files.write("dotfiles/.claude.json.swp", b"{\"a\":1}");
        fs::rename(
            files.path("dotfiles/.claude.json.swp"),
            files.path("dotfiles/claude.json"),
        )
        .expect("the target is replaced");
        noticed(&mut watcher, "a save to the target").await;

        files.write("other/claude.json", b"{\"b\":2}");
        files
            .path("config/claude/settings.json")
            .replace_symlink(&files.path("other/claude.json"))
            .expect("the link is replaced");
        noticed(&mut watcher, "a new link").await;

        files.write("other/claude.json", b"{\"c\":3}");
        noticed(&mut watcher, "a write to the new target").await;

        files.write("dotfiles/claude.json", b"{\"d\":4}");
        not_noticed(&mut watcher, "a write to the old target").await;
    }
}
