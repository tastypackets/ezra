use ezra::inbound::store::{DeliveryScope, DeliveryState, RoutingOutcome, StoreError};

use super::*;

pub(super) struct GitHubDiscussion<'a> {
    pub repository: &'a str,
    pub repository_id: NonZeroU64,
    pub number: NonZeroU64,
    pub workspace: &'a str,
}

impl InboundRuntime {
    pub(super) async fn admit_github_event(
        &self,
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        sender: &impl MessageSender,
        settings: &InboundSettings,
        event: &InboundEvent,
        discussion: GitHubDiscussion<'_>,
    ) -> Result<Admission, PollError> {
        match self.store.insert(event, settings.queue_limits()).await? {
            InsertOutcome::QueueFull => return Err(PollError::QueueFull),
            InsertOutcome::Expired => return Ok(Admission::Expired),
            InsertOutcome::Inserted | InsertOutcome::Duplicate => {}
        }
        let event = self
            .store
            .get(&event.key)
            .await?
            .ok_or(StoreError::DeliveryChanged)?;
        match self.store.delivery_state(&event.key).await? {
            Some(DeliveryState::Delivered) => return Ok(Admission::Accepted),
            Some(DeliveryState::Uncertain) => return Ok(Admission::Uncertain),
            Some(DeliveryState::Delivering) => return Ok(Admission::Deferred),
            Some(DeliveryState::Failed | DeliveryState::Expired) => return Ok(Admission::Expired),
            _ => {}
        }
        let related = if event.new_chat
            || self
                .store
                .find_binding(&event.key.conversation)
                .await?
                .is_some()
        {
            Vec::new()
        } else {
            source
                .linked_discussions(
                    discussion.repository,
                    discussion.repository_id,
                    discussion.number,
                )
                .await?
        };
        let routing = self
            .store
            .claim_routing(
                &event.key,
                &related,
                DeliveryScope {
                    host_id: &self.host_id,
                    agent: "codex",
                },
                discussion.workspace,
            )
            .await?;
        let workspace = match routing {
            RoutingOutcome::Ready => return Ok(Admission::Accepted),
            RoutingOutcome::Deferred => return Ok(Admission::Deferred),
            RoutingOutcome::Uncertain => return Ok(Admission::Uncertain),
            RoutingOutcome::Create { workspace } => workspace,
        };
        match sender
            .create_chat(&workspace, event.chat_name.as_deref())
            .await
        {
            Ok(chat_id) => {
                self.store
                    .finish_routing(
                        &event.key,
                        &SessionTarget {
                            host_id: self.host_id.clone(),
                            agent: "codex".to_owned(),
                            chat_id,
                            workspace,
                        },
                    )
                    .await?;
                tracing::info!(delivery_id = %event.key.delivery_id(), new_chat = event.new_chat, "GitHub trigger attached to a new chat");
            }
            Err(MessageSendError::Unavailable) => {
                self.store
                    .finish_delivery(&event.key, DeliveryOutcome::Pending, None)
                    .await?;
                return Err(PollError::Unavailable);
            }
            Err(error) => {
                self.store
                    .finish_delivery(&event.key, DeliveryOutcome::Failed, None)
                    .await?;
                if let Some(host) = event.key.conversation.source.strip_prefix("github:")
                    && let Some(mut feedback) = GitHubFeedback::from_event(&event, host)
                {
                    feedback.status = CommentStatus::Failed;
                    self.queue_github_feedback(feedback).await;
                }
                tracing::warn!(delivery_id = %event.key.delivery_id(), %error, "GitHub chat creation failed before submission");
                return Ok(Admission::Failed);
            }
        }
        Ok(Admission::Accepted)
    }
}
