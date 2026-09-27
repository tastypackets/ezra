use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How Codex serves this box to the ChatGPT app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct CodexRemoteSettings {
    /// Serve this box to the ChatGPT app while Codex is signed in with ChatGPT.
    #[schema(required = true)]
    pub enabled: bool,
    /// The sandbox Codex runs commands in.
    #[schema(required = true)]
    pub sandbox: CodexSandbox,
    /// When Codex asks for approval.
    #[schema(required = true)]
    pub approvals: CodexApprovals,
}

impl Default for CodexRemoteSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sandbox: CodexSandbox::default(),
            approvals: CodexApprovals::default(),
        }
    }
}

/// What Codex's commands can change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CodexSandbox {
    /// Commands can read files but not change them.
    ReadOnly,
    /// Commands can write in the chat's folder, /tmp and $TMPDIR, with network off by default.
    WorkspaceWrite,
    /// No sandbox.
    #[default]
    DangerFullAccess,
}

/// When Codex asks for approval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CodexApprovals {
    /// Codex asks when it decides it needs to.
    #[default]
    OnRequest,
    /// Codex never asks.
    Never,
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde::de::DeserializeOwned;

    use super::*;

    const SANDBOXES: [(CodexSandbox, &str); 3] = [
        (CodexSandbox::ReadOnly, "read-only"),
        (CodexSandbox::WorkspaceWrite, "workspace-write"),
        (CodexSandbox::DangerFullAccess, "danger-full-access"),
    ];
    const APPROVALS: [(CodexApprovals, &str); 2] = [
        (CodexApprovals::OnRequest, "on-request"),
        (CodexApprovals::Never, "never"),
    ];

    fn round_trips<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T, name: &str) {
        let json = format!("\"{name}\"");
        assert_eq!(
            serde_json::to_string(&value).expect("value serializes"),
            json
        );
        assert_eq!(
            serde_json::from_str::<T>(&json).expect("value parses"),
            value
        );
    }

    #[test]
    fn every_value_is_named_in_kebab_case() {
        for (sandbox, name) in SANDBOXES {
            round_trips(sandbox, name);
        }
        for (approvals, name) in APPROVALS {
            round_trips(approvals, name);
        }
    }

    #[test]
    fn values_ezra_does_not_offer_are_rejected() {
        for approvals in ["untrusted", "on-failure", "Never", "OnRequest"] {
            assert!(
                serde_json::from_str::<CodexApprovals>(&format!("\"{approvals}\"")).is_err(),
                "{approvals}"
            );
        }
        assert!(
            serde_json::from_str::<CodexApprovals>(
                r#"{"granular":{"mcp_elicitations":true,"rules":true,"sandbox_approval":true}}"#
            )
            .is_err()
        );
        for sandbox in ["ReadOnly", "read_only", "danger_full_access"] {
            assert!(
                serde_json::from_str::<CodexSandbox>(&format!("\"{sandbox}\"")).is_err(),
                "{sandbox}"
            );
        }
    }

    #[test]
    fn the_defaults_serve_with_no_sandbox_asking_on_request() {
        assert_eq!(
            CodexRemoteSettings::default(),
            CodexRemoteSettings {
                enabled: true,
                sandbox: CodexSandbox::DangerFullAccess,
                approvals: CodexApprovals::OnRequest,
            }
        );
        assert_eq!(
            serde_json::from_str::<CodexRemoteSettings>("{}").expect("empty settings parse"),
            CodexRemoteSettings::default()
        );
    }
}
