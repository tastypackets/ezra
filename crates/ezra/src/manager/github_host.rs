use std::env;
use std::fmt;

const GH_HOST_VARIABLE: &str = "GH_HOST";
const DOTCOM: &str = "github.com";
const TENANCY_SUFFIX: &str = ".ghe.com";
const DOTCOM_TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];
const ENTERPRISE_TOKEN_VARIABLES: [&str; 2] = ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"];

/// The GitHub instance gh signs in to, from `GH_HOST`, or github.com when it is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubHost(String);

impl GitHubHost {
    pub fn from_environment() -> Self {
        let host = Self::from_value(&env::var(GH_HOST_VARIABLE).unwrap_or_default());
        if !host.is_plain_hostname() {
            tracing::warn!(
                "{GH_HOST_VARIABLE}={:?} is not a lowercase hostname such as ghe.example.com, so gh may not find its sign-in",
                host.0
            );
        }
        host
    }

    /// Kept exactly as given, since gh matches hosts by their exact text.
    pub fn from_value(value: &str) -> Self {
        if value.is_empty() {
            Self::default()
        } else {
            Self(value.to_owned())
        }
    }

    fn is_plain_hostname(&self) -> bool {
        !self.0.starts_with(['-', '.'])
            && !self.0.ends_with('.')
            && !self.0.contains("..")
            && self.0.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || ".-".contains(character)
            })
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
        assert!(GitHubHost::default().is_dotcom());
    }

    #[test]
    fn hosts_are_kept_as_given() {
        for value in [
            "ghe.example.com",
            "GHE.Example.com",
            "api.github.com",
            " ghe.example.com",
        ] {
            assert_eq!(GitHubHost::from_value(value).as_str(), value, "{value}");
        }
    }

    #[test]
    fn only_lowercase_hostnames_are_plain() {
        for (value, plain) in [
            ("ghe.example.com", true),
            ("ghe-1.example.com", true),
            ("GHE.example.com", false),
            (" ghe.example.com", false),
            ("https://ghe.example.com", false),
            ("ghe.example.com:8443", false),
            ("-ghe.example.com", false),
            ("ghe..example.com", false),
            ("ghe.example.com.", false),
        ] {
            assert_eq!(
                GitHubHost::from_value(value).is_plain_hostname(),
                plain,
                "{value}"
            );
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
