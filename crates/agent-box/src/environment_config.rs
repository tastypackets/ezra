use config::{Config, ConfigError, Environment, Map};
use serde::de::DeserializeOwned;

/// Typed settings read from environment variables.
///
/// Each field is named after its variable in lowercase, so `agent_sudo` reads `AGENT_SUDO`.
/// An empty variable counts as unset, and a value the field's type rejects is an error.
pub trait FromEnvironment: DeserializeOwned {
    fn from_environment() -> Result<Self, ConfigError> {
        Self::from_variables(None)
    }

    /// Reads these variables instead of the process environment.
    fn from_variables(variables: Option<Map<String, String>>) -> Result<Self, ConfigError> {
        Config::builder()
            .add_source(Environment::default().ignore_empty(true).source(variables))
            .build()?
            .try_deserialize()
    }
}

#[cfg(test)]
pub fn variables(pairs: &[(&str, &str)]) -> Option<Map<String, String>> {
    Some(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    )
}
