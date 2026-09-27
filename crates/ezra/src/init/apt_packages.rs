use std::collections::HashSet;
use std::io::{self, Write};
use std::process::{Command, Stdio};

use serde::Deserialize;

use super::root_command::RootCommandExt;
use crate::process_ext::{CommandStatusExt, OutputExt};

pub const APT_PACKAGES_VARIABLE: &str = "EZRA_APT_PACKAGES";
const APT_GET_OPTIONS: [&str; 8] = [
    "-q",
    "-o",
    "APT::Color=0",
    "-o",
    "Dpkg::Use-Pty=0",
    "-o",
    "Dpkg::Options::=--force-confold",
    "--no-install-recommends",
];

/// Package arguments for apt-get from `EZRA_APT_PACKAGES`, separated by whitespace or commas.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub struct RequestedPackages(Vec<String>);

impl From<String> for RequestedPackages {
    fn from(list: String) -> Self {
        Self(
            list.split(|character: char| character.is_whitespace() || character == ',')
                .filter(|package| !package.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }
}

impl RequestedPackages {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> &[String] {
        &self.0
    }

    /// Runs as root, before switching to the agent user.
    pub fn install_missing(&self) {
        if self.is_empty() {
            return;
        }
        let mut database = DpkgDatabase::query();
        if database.as_ref().is_ok_and(|database| database.interrupted) {
            tracing::info!("finishing an interrupted package installation");
            if let Err(error) = Command::new("dpkg")
                .apart_from_agent()
                .args(["--configure", "-a"])
                .run_checked()
            {
                tracing::warn!("dpkg --configure -a failed ({error})");
            }
            database = DpkgDatabase::query();
        }
        let missing_names = match database {
            Ok(database) => database.missing(&self.0),
            Err(error) => {
                tracing::warn!(
                    "could not read the dpkg database ({error}), so {APT_PACKAGES_VARIABLE} was skipped"
                );
                return;
            }
        };
        if !missing_names.is_empty() {
            AptGet::install(&missing_names);
        }
    }
}

/// apt-get, non-interactive and quiet enough for a container log.
struct AptGet;

impl AptGet {
    fn install(missing_names: &[String]) {
        tracing::info!(
            "installing {} from {APT_PACKAGES_VARIABLE}",
            missing_names.join(" ")
        );
        if let Err(error) = Self::command().arg("update").run_checked() {
            tracing::warn!(
                "apt-get update failed ({error}). Not installed: {}",
                missing_names.join(" ")
            );
            return;
        }
        if Self::command()
            .arg("install")
            .args(missing_names)
            .run_checked()
            .is_ok()
        {
            return;
        }

        tracing::info!("retrying each package on its own");
        let failed_names: Vec<&str> = missing_names
            .iter()
            .filter(|name| {
                Self::command()
                    .arg("install")
                    .arg(name)
                    .run_checked()
                    .is_err()
            })
            .map(String::as_str)
            .collect();
        if !failed_names.is_empty() {
            tracing::warn!(
                "could not install {} from {APT_PACKAGES_VARIABLE}, see the apt-get output above",
                failed_names.join(" ")
            );
        }
    }

    fn command() -> Command {
        let mut command = Command::new("apt-get");
        command
            .apart_from_agent()
            .env("DEBIAN_FRONTEND", "noninteractive")
            .args(APT_GET_OPTIONS)
            .arg("--yes");
        command
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DpkgDatabase {
    /// Installed package names plus the virtual names they provide.
    satisfied_names: HashSet<String>,
    interrupted: bool,
}

impl DpkgDatabase {
    fn query() -> io::Result<Self> {
        let output = Command::new("dpkg-query")
            .apart_from_agent()
            .args([
                "--show",
                "--showformat=${db:Status-Status}\t${Package}\t${Provides}\n",
            ])
            .stdin(Stdio::null())
            .output()?;
        io::stderr().write_all(&output.stderr)?;
        if !output.status.success() {
            return Err(io::Error::other(format!("dpkg-query {}", output.status)));
        }
        Ok(Self::parse(&output.stdout_text()))
    }

    /// Parses `dpkg-query --show --showformat='${db:Status-Status}\t${Package}\t${Provides}\n'`.
    fn parse(listing: &str) -> Self {
        let mut database = Self::default();
        for line in listing.lines() {
            let mut fields = line.split('\t');
            let (Some(status), Some(package)) = (fields.next(), fields.next()) else {
                continue;
            };
            match status {
                "installed" | "triggers-awaited" | "triggers-pending" => {
                    database.satisfied_names.insert(package.to_owned());
                    let provided_names = fields
                        .next()
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|provided| provided.split_whitespace().next());
                    database
                        .satisfied_names
                        .extend(provided_names.map(str::to_owned));
                }
                "half-installed" | "unpacked" | "half-configured" => {}
                _ => continue,
            }
            if status != "installed" {
                database.interrupted = true;
            }
        }
        database
    }

    /// Compares only the name in arguments like `jq=1.7`, `hello/resolute` or `libc6:i386`.
    fn missing(&self, requested_packages: &[String]) -> Vec<String> {
        requested_packages
            .iter()
            .filter(|package| {
                let name = package.split(['=', '/', ':']).next().unwrap_or_default();
                !self.satisfied_names.contains(name)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn packages_are_separated_by_spaces_or_commas() {
        assert_eq!(
            RequestedPackages::from("hello, jq=1.7  ripgrep,,hello/resolute\n".to_owned()).names(),
            ["hello", "jq=1.7", "ripgrep", "hello/resolute"]
        );
    }

    #[test]
    fn empty_value_requests_nothing() {
        assert!(RequestedPackages::from("  , ".to_owned()).is_empty());
    }

    #[test]
    fn versions_releases_and_architectures_are_ignored_when_checking_installed() {
        let database = DpkgDatabase::parse("installed\tjq\t\ninstalled\thello\t\n");
        assert!(
            database
                .missing(&names(&["jq=1.7", "hello/resolute", "jq:amd64"]))
                .is_empty()
        );
        assert_eq!(database.missing(&names(&["tree=2.2"])), ["tree=2.2"]);
    }

    #[test]
    fn installed_packages_and_what_they_provide_are_satisfied() {
        let database = DpkgDatabase::parse(
            "installed\tbash\t\n\
             installed\tmawk\tawk\n\
             installed\tapt\tapt-transport-https (= 3.2.0)\n\
             installed\tcoreutils-from-uutils\tcoreutils, coreutils-from\n",
        );
        assert_eq!(
            database.missing(&names(&[
                "bash",
                "awk",
                "apt-transport-https",
                "coreutils",
                "hello"
            ])),
            ["hello"]
        );
        assert!(!database.interrupted);
    }

    #[test]
    fn removed_or_unknown_packages_are_missing() {
        let database = DpkgDatabase::parse(
            "config-files\tnano\teditor\n\
             not-installed\tvim\teditor\n",
        );
        assert_eq!(
            database.missing(&names(&["nano", "vim", "editor"])),
            ["nano", "vim", "editor"]
        );
        assert!(!database.interrupted);
    }

    #[test]
    fn half_finished_packages_are_missing_and_mark_an_interruption() {
        for status in ["half-installed", "unpacked", "half-configured"] {
            let database = DpkgDatabase::parse(&format!("{status}\thello\t\n"));
            assert_eq!(database.missing(&names(&["hello"])), ["hello"]);
            assert!(database.interrupted, "{status}");
        }
    }

    #[test]
    fn pending_triggers_count_as_installed_but_mark_an_interruption() {
        let database = DpkgDatabase::parse("triggers-pending\tman-db\t\n");
        assert!(database.missing(&names(&["man-db"])).is_empty());
        assert!(database.interrupted);
    }
}
