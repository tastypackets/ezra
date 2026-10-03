use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use nix::fcntl::OFlag;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

const CREDENTIALS_LIMIT: u64 = 1024 * 1024;
const REREAD_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
const SESSIONS_SCOPE: &str = "user:sessions:claude_code";

/// Claude Code's claude.ai sign-in, read again on every use so that a token the CLI refreshed is
/// picked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLogin {
    credentials: PathBuf,
}

/// Why there is no usable token.
#[derive(Debug, thiserror::Error)]
pub enum LoginProblem {
    #[error("could not read Claude Code's credentials: {0}")]
    Unreadable(io::Error),
    #[error("Claude Code's credentials are not in the expected format")]
    Unparsable,
    #[error("Claude Code is not signed in to claude.ai")]
    SignedOut,
    #[error("Claude Code's sign-in cannot manage sessions")]
    MissingScope,
}

/// A bearer token, kept out of `Debug` output.
pub struct AccessToken(HeaderValue);

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthCredentials>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthCredentials {
    access_token: Option<String>,
    scopes: Option<Vec<String>>,
}

impl ClaudeLogin {
    /// Where Claude Code keeps its sign-in: `CLAUDE_CONFIG_DIR` when set, otherwise `~/.claude`.
    pub fn locate(config_directory: Option<&Path>, home: &Path) -> Self {
        Self {
            credentials: config_directory
                .map_or_else(|| home.join(".claude"), Path::to_path_buf)
                .join(".credentials.json"),
        }
    }

    /// Reads the file again after a short pause when it could not be read or parsed, in case
    /// Claude Code was rewriting it.
    pub async fn access_token(&self) -> Result<AccessToken, LoginProblem> {
        match self.read_access_token().await {
            Err(LoginProblem::Unreadable(_) | LoginProblem::Unparsable) => {
                tokio::time::sleep(REREAD_PAUSE).await;
                self.read_access_token().await
            }
            token => token,
        }
    }

    async fn read_access_token(&self) -> Result<AccessToken, LoginProblem> {
        let bytes = ClaudeFile(&self.credentials)
            .read(CREDENTIALS_LIMIT)
            .await
            .map_err(LoginProblem::Unreadable)?;
        let file: CredentialsFile =
            serde_json::from_slice(&bytes).map_err(|_| LoginProblem::Unparsable)?;
        let oauth = file.oauth.ok_or(LoginProblem::SignedOut)?;
        if oauth
            .scopes
            .is_some_and(|scopes| !scopes.iter().any(|scope| scope == SESSIONS_SCOPE))
        {
            return Err(LoginProblem::MissingScope);
        }
        let token = oauth
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or(LoginProblem::SignedOut)?;
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| LoginProblem::Unparsable)?;
        bearer.set_sensitive(true);
        Ok(AccessToken(bearer))
    }
}

impl AccessToken {
    pub fn bearer(&self) -> HeaderValue {
        self.0.clone()
    }
}

#[cfg(test)]
impl AccessToken {
    pub fn for_test(token: &str) -> Self {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}")).expect("a valid token");
        bearer.set_sensitive(true);
        Self(bearer)
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(..)")
    }
}

/// A file Claude Code writes in a directory agents can also write to.
struct ClaudeFile<'path>(&'path Path);

impl ClaudeFile<'_> {
    /// Reads at most `limit` bytes, and only from a regular file. Opening does not block on a
    /// FIFO.
    async fn read(&self, limit: u64) -> io::Result<Vec<u8>> {
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(OFlag::O_NONBLOCK.bits())
            .open(self.0)
            .await?;
        if !file.metadata().await?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("larger than {limit} bytes"),
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const TOKEN: &str = "test-access-token";

    fn login() -> (TempDir, ClaudeLogin) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let login = ClaudeLogin::locate(Some(directory.path()), Path::new("/nonexistent"));
        (directory, login)
    }

    fn write(path: &Path, contents: &str) {
        fs::write(path, contents).expect("file is written");
    }

    #[test]
    fn the_sign_in_is_in_the_config_directory_or_home() {
        assert_eq!(
            ClaudeLogin::locate(Some(Path::new("/config")), Path::new("/home")),
            ClaudeLogin {
                credentials: PathBuf::from("/config/.credentials.json"),
            }
        );
        assert_eq!(
            ClaudeLogin::locate(None, Path::new("/home")),
            ClaudeLogin {
                credentials: PathBuf::from("/home/.claude/.credentials.json"),
            }
        );
    }

    #[tokio::test]
    async fn the_token_is_read_again_on_every_use() {
        let (_directory, login) = login();
        write(
            &login.credentials,
            r#"{"claudeAiOauth":{"accessToken":"first","scopes":["user:inference","user:sessions:claude_code"],"expiresAt":1},"futureField":{}}"#,
        );
        let first = login.access_token().await.expect("token is read");
        assert_eq!(first.bearer(), "Bearer first");
        assert!(first.bearer().is_sensitive());

        write(
            &login.credentials,
            r#"{"claudeAiOauth":{"accessToken":"refreshed"}}"#,
        );
        assert_eq!(
            login.access_token().await.expect("token is read").bearer(),
            "Bearer refreshed"
        );
    }

    #[tokio::test]
    async fn credentials_caught_while_they_are_rewritten_are_read_again() {
        let (_directory, login) = login();
        write(&login.credentials, r#"{"claudeAiOauth":{"accessTo"#);
        let credentials = login.credentials.clone();
        let rewrite = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            write(
                &credentials,
                r#"{"claudeAiOauth":{"accessToken":"rewritten"}}"#,
            );
        });
        assert_eq!(
            login.access_token().await.expect("token is read").bearer(),
            "Bearer rewritten"
        );
        rewrite.await.expect("the file is rewritten");
    }

    #[tokio::test]
    async fn unusable_credentials_name_the_problem_without_their_contents() {
        let (directory, login) = login();
        assert!(matches!(
            login.access_token().await,
            Err(LoginProblem::Unreadable(_))
        ));
        for (contents, expected) in [
            (
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","scopes":["user:inference"]}}}}"#
                ),
                "MissingScope",
            ),
            (
                format!(r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","scopes":"{TOKEN}"}}}}"#),
                "Unparsable",
            ),
            (
                format!(r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}\n"}}}}"#),
                "Unparsable",
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":""}}"#.to_owned(),
                "SignedOut",
            ),
            (r#"{"claudeAiOauth":{}}"#.to_owned(), "SignedOut"),
            ("{}".to_owned(), "SignedOut"),
            (format!("not json {TOKEN}"), "Unparsable"),
        ] {
            write(&login.credentials, &contents);
            let problem = login
                .access_token()
                .await
                .expect_err("the credentials are unusable");
            assert!(format!("{problem:?}").starts_with(expected), "{problem:?}");
            assert!(!problem.to_string().contains(TOKEN));
        }

        fs::remove_file(&login.credentials).expect("credentials are removed");
        nix::unistd::mkfifo(&login.credentials, nix::sys::stat::Mode::S_IRWXU)
            .expect("a FIFO is created");
        assert!(matches!(
            login.access_token().await,
            Err(LoginProblem::Unreadable(_))
        ));

        fs::remove_file(&login.credentials).expect("FIFO is removed");
        let large = directory.path().join("large");
        fs::write(&large, vec![b' '; 2 * 1024 * 1024]).expect("large file is written");
        std::os::unix::fs::symlink(&large, &login.credentials).expect("link is created");
        assert!(matches!(
            login.access_token().await,
            Err(LoginProblem::Unreadable(_))
        ));
    }

    #[test]
    fn tokens_stay_out_of_debug_output() {
        let mut header = HeaderValue::from_static("Bearer secret");
        header.set_sensitive(true);
        let token = AccessToken(header);
        assert_eq!(format!("{token:?}"), "AccessToken(..)");
    }
}
