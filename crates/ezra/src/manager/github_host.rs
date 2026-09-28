use std::env;
use std::fmt;

pub const GITHUB_HOST_VARIABLE: &str = "GH_HOST";
const DOTCOM: &str = "github.com";
const DOTCOM_SUBDOMAIN_SUFFIX: &str = ".github.com";
const TENANCY_SUFFIX: &str = ".ghe.com";
const DOTCOM_TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];
const ENTERPRISE_TOKEN_VARIABLES: [&str; 2] = ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"];

/// The GitHub instance gh signs in to, from `GH_HOST`, or github.com when it is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubHost(String);

impl GitHubHost {
    pub fn from_environment() -> Self {
        Self::from_value(&env::var(GITHUB_HOST_VARIABLE).unwrap_or_default())
    }

    /// Lowercased, and a github.com subdomain means github.com, as gh reads it.
    pub fn from_value(value: &str) -> Self {
        let host = value.trim().to_ascii_lowercase();
        if host.is_empty() || host.ends_with(DOTCOM_SUBDOMAIN_SUFFIX) {
            Self::default()
        } else {
            Self(host)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_dotcom(&self) -> bool {
        self.0 == DOTCOM
    }

    /// The variables whose token gh uses for this host over any sign-in.
    pub fn token_variables(&self) -> [&'static str; 2] {
        if self.is_dotcom() || self.0.ends_with(TENANCY_SUFFIX) {
            DOTCOM_TOKEN_VARIABLES
        } else {
            ENTERPRISE_TOKEN_VARIABLES
        }
    }

    /// "GitHub" for github.com, otherwise the host.
    pub fn display_name(&self) -> &str {
        if self.is_dotcom() { "GitHub" } else { &self.0 }
    }

    pub fn https_url(&self) -> String {
        format!("https://{}", self.0)
    }
}

impl Default for GitHubHost {
    fn default() -> Self {
        Self(DOTCOM.to_owned())
    }
}

impl fmt::Display for GitHubHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_means_github_com() {
        assert_eq!(GitHubHost::from_value(""), GitHubHost::default());
        assert_eq!(GitHubHost::from_value("  "), GitHubHost::default());
        assert!(GitHubHost::default().is_dotcom());
    }

    #[test]
    fn hosts_are_read_as_gh_reads_them() {
        for (value, host) in [
            ("GHE.Example.com", "ghe.example.com"),
            (" ghe.example.com\n", "ghe.example.com"),
            ("api.github.com", "github.com"),
            ("acme.ghe.com", "acme.ghe.com"),
        ] {
            assert_eq!(GitHubHost::from_value(value).as_str(), host, "{value}");
        }
    }

    #[test]
    fn enterprise_server_hosts_use_the_enterprise_token() {
        for (host, variables) in [
            ("github.com", DOTCOM_TOKEN_VARIABLES),
            ("acme.ghe.com", DOTCOM_TOKEN_VARIABLES),
            ("ghe.example.com", ENTERPRISE_TOKEN_VARIABLES),
        ] {
            assert_eq!(
                GitHubHost::from_value(host).token_variables(),
                variables,
                "{host}"
            );
        }
    }
}
