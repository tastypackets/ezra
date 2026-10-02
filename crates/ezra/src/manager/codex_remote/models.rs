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
