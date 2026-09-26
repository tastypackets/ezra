use serde::Deserialize;

use super::apt_packages::RequestedPackages;
use super::sudo::SudoPolicy;
use crate::environment_config::FromEnvironment;

/// Settings for `agent-box init`, read only from the environment.
///
/// Init runs as root, and the settings file on /config is writable by the agent, so it never reads that file.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct InitConfig {
    #[serde(rename = "agent_sudo")]
    pub sudo_policy: SudoPolicy,
    pub apt_packages: RequestedPackages,
}

impl FromEnvironment for InitConfig {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_config::variables;

    fn config(pairs: &[(&str, &str)]) -> Result<InitConfig, config::ConfigError> {
        InitConfig::from_variables(variables(pairs))
    }

    #[test]
    fn unset_or_empty_variables_mean_defaults() {
        assert_eq!(config(&[]).expect("defaults load"), InitConfig::default());
        let empty = config(&[("AGENT_SUDO", ""), ("APT_PACKAGES", "")]).expect("empty loads");
        assert_eq!(empty, InitConfig::default());
    }

    #[test]
    fn sudo_policy_is_full_or_off() {
        let full = config(&[("AGENT_SUDO", "full")]).expect("full loads");
        assert_eq!(full.sudo_policy, SudoPolicy::Full);
        let off = config(&[("AGENT_SUDO", "off")]).expect("off loads");
        assert_eq!(off.sudo_policy, SudoPolicy::Off);
        for value in ["FULL", "on", "true", "1"] {
            let error = config(&[("AGENT_SUDO", value)]).expect_err("value is rejected");
            assert!(error.to_string().contains("agent_sudo"), "{error}");
        }
    }

    #[test]
    fn apt_packages_are_split_into_arguments() {
        let requested = config(&[("APT_PACKAGES", "hello, jq=1.7")]).expect("packages load");
        assert_eq!(requested.apt_packages.names(), ["hello", "jq=1.7"]);
    }

    #[test]
    fn unrelated_variables_are_ignored() {
        let unrelated = config(&[("PATH", "/usr/bin"), ("HOME", "/root")]).expect("loads");
        assert_eq!(unrelated, InitConfig::default());
    }
}
