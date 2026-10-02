use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

use crate::manager::folders::ProjectsDirectory;

impl ProjectsDirectory {
    pub async fn github_workspaces(
        &self,
        host: &str,
        _settings: &ezra::inbound::github::GitHubSettings,
    ) -> io::Result<BTreeMap<String, PathBuf>> {
        let projects = self.clone();
        let folders = tokio::task::spawn_blocking(move || projects.folders())
            .await
            .map_err(io::Error::other)??;
        let mut workspaces = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for folder in folders {
            let Some(origin) = folder.git.and_then(|git| git.repository) else {
                continue;
            };
            let Some(repository) = origin.github_repository(host) else {
                continue;
            };
            if workspaces
                .insert(repository.clone(), self.folder(&folder.name))
                .is_some()
            {
                ambiguous.insert(repository);
            }
        }
        for repository in ambiguous {
            workspaces.insert(repository.clone(), PathBuf::from("/home/dev"));
            tracing::warn!(%repository, "multiple local checkouts match the GitHub trigger repository, using the home directory");
        }
        Ok(workspaces)
    }
}

pub(crate) trait GitHubOriginExt {
    fn github_repository(&self, host: &str) -> Option<String>;
}

impl GitHubOriginExt for str {
    fn github_repository(&self, host: &str) -> Option<String> {
        let repository = if self.contains("://") {
            let url = reqwest::Url::parse(self).ok()?;
            if !matches!(url.scheme(), "http" | "https" | "ssh")
                || !url.host_str()?.eq_ignore_ascii_case(host)
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            url.path().trim_start_matches('/').to_owned()
        } else {
            let (authority, path) = self.split_once(':')?;
            let remote_host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            if !remote_host.eq_ignore_ascii_case(host) {
                return None;
            }
            path.to_owned()
        };
        let repository = repository.trim_end_matches('/').trim_end_matches(".git");
        super::GitTools::validate_repository(repository).ok()?;
        Some(repository.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_repository_origins_on_the_configured_host_match() {
        for origin in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
        ] {
            assert_eq!(
                origin.github_repository("github.com").as_deref(),
                Some("owner/repo")
            );
        }
        for origin in [
            "/local/repo",
            "https://other.example/owner/repo",
            "git@other.example:owner/repo",
            "https://github.com/owner/repo/extra",
            "https://github.com/owner/repo?other=1",
        ] {
            assert!(origin.github_repository("github.com").is_none());
        }
    }

    #[tokio::test]
    async fn workspace_selection_requires_unique_local_repositories() {
        let directory = tempfile::tempdir().expect("projects");
        for folder in ["first", "duplicate", "disabled"] {
            let git_directory = directory.path().join(folder).join(".git");
            std::fs::create_dir_all(&git_directory).expect("git directory");
            std::fs::write(git_directory.join("HEAD"), "ref: refs/heads/main").expect("head");
            let repository = if folder == "disabled" {
                "disabled"
            } else {
                "repo"
            };
            std::fs::write(
                git_directory.join("config"),
                format!("[remote \"origin\"]\nurl = https://github.com/owner/{repository}.git\n"),
            )
            .expect("origin");
        }
        let projects = ProjectsDirectory(directory.path().to_owned());
        let settings = ezra::inbound::github::GitHubSettings::default();
        let workspaces = projects
            .github_workspaces("github.com", &settings)
            .await
            .expect("scan");
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces["owner/repo"], PathBuf::from("/home/dev"));
        assert!(workspaces.contains_key("owner/disabled"));
        std::fs::remove_dir_all(directory.path().join("duplicate"))
            .expect("remove duplicate fixture");
        let workspaces = projects
            .github_workspaces("github.com", &settings)
            .await
            .expect("scan");
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces["owner/repo"], directory.path().join("first"));
    }
}
