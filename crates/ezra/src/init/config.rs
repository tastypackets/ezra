use serde::Deserialize;

use super::apt_packages::RequestedPackages;
use super::empty_mounts::EmptyMountOwnership;
use super::sudo::SudoPolicy;
use crate::environment_config::FromEnvironment;

/// Settings for `ezra init`, read only from the environment.
///
/// Init runs as root, and the settings file on /config is writable by the agent, so it never reads that file.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct InitConfig {
    #[serde(rename = "sudo")]
    pub sudo_policy: SudoPolicy,
    pub apt_packages: RequestedPackages,
    #[serde(rename = "chown_empty_mounts")]
    pub empty_mount_ownership: EmptyMountOwnership,
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
        let empty = config(&[("EZRA_SUDO", ""), ("EZRA_APT_PACKAGES", "")]).expect("empty loads");
        assert_eq!(empty, InitConfig::default());
    }

    #[test]
    fn sudo_policy_is_full_or_off() {
        let full = config(&[("EZRA_SUDO", "full")]).expect("full loads");
        assert_eq!(full.sudo_policy, SudoPolicy::Full);
        let off = config(&[("EZRA_SUDO", "off")]).expect("off loads");
        assert_eq!(off.sudo_policy, SudoPolicy::Off);
        for value in ["FULL", "on", "true", "1"] {
            let error = config(&[("EZRA_SUDO", value)]).expect_err("value is rejected");
            assert!(error.to_string().contains("`sudo`"), "{error}");
        }
    }

    #[test]
    fn apt_packages_are_split_into_arguments() {
        let requested = config(&[("EZRA_APT_PACKAGES", "hello, jq=1.7")]).expect("packages load");
        assert_eq!(requested.apt_packages.names(), ["hello", "jq=1.7"]);
    }

    #[test]
    fn empty_mount_ownership_is_on_or_off() {
        assert_eq!(
            config(&[]).expect("defaults load").empty_mount_ownership,
            EmptyMountOwnership::On
        );
        let off = config(&[("EZRA_CHOWN_EMPTY_MOUNTS", "off")]).expect("off loads");
        assert_eq!(off.empty_mount_ownership, EmptyMountOwnership::Off);
        let error = config(&[("EZRA_CHOWN_EMPTY_MOUNTS", "false")]).expect_err("value is rejected");
        assert!(
            error.to_string().contains("`chown_empty_mounts`"),
            "{error}"
        );
    }

    #[test]
    fn unrelated_variables_are_ignored() {
        let unrelated = config(&[("PATH", "/usr/bin"), ("HOME", "/root")]).expect("loads");
        assert_eq!(unrelated, InitConfig::default());
    }
}
