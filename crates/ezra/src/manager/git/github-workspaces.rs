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
