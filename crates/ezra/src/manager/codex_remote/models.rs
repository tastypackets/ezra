use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use utoipa::ToSchema;

use super::CodexRemote;
use super::control::{ControlError, ControlRequest};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct CodexModel {
    pub model: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub is_default: bool,
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<CodexEffort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct CodexEffort {
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListModels {
    cursor: Option<String>,
    limit: u32,
    include_hidden: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    data: Vec<CodexModel>,
    next_cursor: Option<String>,
}

impl ControlRequest for ListModels {
    const METHOD: &'static str = "model/list";
    type Response = ModelPage;
}

impl CodexRemote {
    pub async fn models(&self) -> Result<Vec<CodexModel>, ControlError> {
        let client = self.client().ok_or(ControlError::Closed)?;
        timeout(self.budget.request, async {
            let mut models = Vec::new();
            let mut cursor = None;
            let mut seen_cursors = BTreeSet::new();
            loop {
                let page = client
                    .request(ListModels {
                        cursor,
                        limit: 100,
                        include_hidden: false,
                    })
                    .await?;
                models.extend(page.data.into_iter().filter(|model| !model.hidden));
                let Some(next) = page.next_cursor else {
                    return Ok(models);
                };
                if seen_cursors.len() >= 10 || !seen_cursors.insert(next.clone()) {
                    return Err(ControlError::Io(std::io::Error::other(
                        "Codex model pagination did not finish",
                    )));
                }
                cursor = Some(next);
            }
        })
        .await
        .map_err(|_| ControlError::TimedOut)?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use nix::unistd::Pid;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::manager::codex_remote::control::{ControlClient, ControlEvent, ControlSocket};
    use crate::manager::codex_remote::fake::{FakeControlServer, Reply};
    use crate::manager::codex_remote::{ExpectedPeer, ServerBudget};
    use crate::manager::events::Events;
    use crate::manager::supervision::ServerLog;

    struct Fixture {
        remote: CodexRemote,
        fake: FakeControlServer,
        _events: mpsc::UnboundedReceiver<ControlEvent>,
        _home: TempDir,
    }

    impl Fixture {
        async fn connected() -> Self {
            let home = tempfile::tempdir_in("/tmp").expect("Codex home");
            let fake = FakeControlServer::bind(home.path());
            let budget = ServerBudget {
                request: Duration::from_secs(1),
                ..ServerBudget::default()
            };
            let (client, events) = ControlClient::connect(
                &ControlSocket::of(home.path()),
                Pid::this(),
                home.path(),
                &budget,
            )
            .await
            .expect("control connection");
            let remote = CodexRemote::new(
                Events::default(),
                ServerLog(home.path().join("logs")),
                budget,
                ExpectedPeer::Child,
            );
            remote.with_control(|control| control.client = Some(Arc::new(client)));
            Self {
                remote,
                fake,
                _events: events,
                _home: home,
            }
        }
    }

    #[tokio::test]
    async fn discovery_paginates_and_accepts_unknown_fields_and_efforts() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply("model/list", [
            Reply::Result(json!({"data": [{"model":"first", "displayName":"First", "supportedReasoningEfforts":[{"reasoningEffort":"future-effort", "newField":true}], "newCapability":{}}], "nextCursor":"next", "extra":true})),
            Reply::Result(json!({"data":[{"model":"hidden", "hidden":true}, {"model":"second", "isDefault":true}], "nextCursor":null})),
        ]);
        let models = fixture.remote.models().await.expect("model catalog");
        assert_eq!(
            models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            models[0].supported_reasoning_efforts[0].reasoning_effort,
            "future-effort"
        );
        let serialized = serde_json::to_value(&models[0]).expect("API JSON");
        assert_eq!(serialized["display_name"], "First");
        assert_eq!(
            fixture.fake.requests_of("model/list"),
            [
                json!({"cursor":null,"limit":100,"includeHidden":false}),
                json!({"cursor":"next","limit":100,"includeHidden":false})
            ]
        );
    }

    #[tokio::test]
    async fn repeated_cursor_and_malformed_catalog_do_not_break_the_connection() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply(
            "model/list",
            [
                Reply::Result(json!({"data":[],"nextCursor":"repeat"})),
                Reply::Result(json!({"data":[],"nextCursor":"repeat"})),
                Reply::Result(json!({"data":[{"model":123}]})),
                Reply::Result(json!({"data":[{"model":"recovered"}]})),
            ],
        );
        assert!(fixture.remote.models().await.is_err());
        assert!(fixture.remote.models().await.is_err());
        assert_eq!(
            fixture
                .remote
                .models()
                .await
                .expect("connection still works")[0]
                .model,
            "recovered"
        );
    }

    #[tokio::test]
    async fn unknown_method_returns_an_operation_error() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply(
            "model/list",
            [Reply::Error {
                code: -32601,
                message: "unsupported".to_owned(),
            }],
        );
        assert!(matches!(
            fixture.remote.models().await,
            Err(ControlError::Codex { code: -32601, .. })
        ));
    }
}
