use super::{DeliveryOutcome, DeliveryScope, EventStore, StoreError};
use crate::inbound::{
    EventKey, InboundEvent, MessageAttempt, MessageReceipt, MessageSendError, MessageSender,
};
use time::OffsetDateTime;

struct DeliveryAttempt<'store> {
    store: &'store EventStore,
    waiting_duration: time::Duration,
}

impl MessageAttempt for DeliveryAttempt<'_> {
    async fn mark_attempted(&self, key: &EventKey) -> Result<(), MessageSendError> {
        let cutoff = OffsetDateTime::now_utc()
            .saturating_sub(self.waiting_duration)
            .unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_events SET attempted_at = COALESCE(attempted_at, unixepoch())
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'delivering'
               AND (attempted_at IS NOT NULL OR received_at > ?4)",
            key.conversation.source,
            key.conversation.subject,
            key.id,
            cutoff,
        )
        .execute(&self.store.pool)
        .await
        .map_err(|error| MessageSendError::Uncertain(error.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(MessageSendError::Uncertain(
                "request is no longer eligible for submission".to_owned(),
            ));
        }
        Ok(())
    }

    async fn mark_rejected(&self, key: &EventKey) -> Result<(), MessageSendError> {
        sqlx::query!(
            "UPDATE inbound_events SET attempted_at = NULL
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'delivering'",
            key.conversation.source, key.conversation.subject, key.id,
        ).execute(&self.store.pool).await
            .map_err(|error| MessageSendError::Uncertain(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum DispatchOutcome {
    Idle,
    Delivered {
        event: EventKey,
        receipt: MessageReceipt,
    },
    Unavailable {
        event: EventKey,
    },
    Uncertain {
        event: EventKey,
        reason: String,
    },
    Failed {
        event: EventKey,
        reason: String,
    },
}

impl EventStore {
    #[tracing::instrument(
        skip_all,
        fields(host_id = scope.host_id, agent = scope.agent, delivery_id = tracing::field::Empty),
        err(level = "error")
    )]
    pub async fn dispatch_next(
        &self,
        scope: DeliveryScope<'_>,
        sender: &impl MessageSender,
    ) -> Result<DispatchOutcome, StoreError> {
        let Some(event) = self.claim_next(Some(scope)).await? else {
            return Ok(DispatchOutcome::Idle);
        };
        self.dispatch_claimed(event, sender, OffsetDateTime::UNIX_EPOCH)
            .await
    }

    pub async fn dispatch_event(
        &self,
        key: &EventKey,
        scope: DeliveryScope<'_>,
        sender: &impl MessageSender,
        cutoff: OffsetDateTime,
    ) -> Result<DispatchOutcome, StoreError> {
        let Some(event) = self.claim_event(key, scope).await? else {
            return Ok(DispatchOutcome::Idle);
        };
        self.dispatch_claimed(event, sender, cutoff).await
    }

    async fn dispatch_claimed(
        &self,
        event: InboundEvent,
        sender: &impl MessageSender,
        cutoff: OffsetDateTime,
    ) -> Result<DispatchOutcome, StoreError> {
        let attempt = DeliveryAttempt {
            store: self,
            waiting_duration: time::Duration::seconds(
                OffsetDateTime::now_utc()
                    .unix_timestamp()
                    .saturating_sub(cutoff.unix_timestamp()),
            ),
        };
        let delivery_id = event.key.delivery_id();
        tracing::Span::current().record("delivery_id", delivery_id.as_str());
        let target = self
            .find_binding(&event.key.conversation)
            .await?
            .ok_or(StoreError::DeliveryChanged)?;
        let initial_context_pending = sqlx::query_scalar!(
            r#"SELECT initial_context_pending AS "initial_context_pending!: bool"
               FROM inbound_sessions WHERE host_id = ?1 AND agent = ?2 AND chat_id = ?3"#,
            target.host_id,
            target.agent,
            target.chat_id,
        )
        .fetch_one(&self.pool)
        .await?;
        let message = if initial_context_pending {
            event.with_initial_context()
        } else {
            event.clone()
        };
        let result = match sender
            .queue_message_tracked(&target.chat_id, &message, &attempt)
            .await
        {
            Err(MessageSendError::NeedsReplacement(reason)) => {
                attempt
                    .mark_rejected(&event.key)
                    .await
                    .map_err(|_| StoreError::DeliveryChanged)?;
                tracing::info!(%reason, "creating replacement chat for inbound delivery");
                match sender
                    .create_chat(&target.workspace, event.chat_name.as_deref())
                    .await
                {
                    Ok(chat_id) => {
                        self.replace_chat_for_delivery(&event.key, &target, &chat_id)
                            .await?;
                        sender
                            .queue_message_tracked(
                                &chat_id,
                                &event.with_initial_context(),
                                &attempt,
                            )
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
            result => result,
        };
        let attempted = sqlx::query_scalar!(
            r#"SELECT attempted_at IS NOT NULL AS "attempted!: bool" FROM inbound_events
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3"#,
            event.key.conversation.source,
            event.key.conversation.subject,
            event.key.id,
        )
        .fetch_one(&self.pool)
        .await?;
        let (state, outcome) = match result {
            Ok(receipt)
                if receipt.delivery_id == event.key.delivery_id()
                    && !receipt.native_message_id.is_empty() =>
            {
                (
                    DeliveryOutcome::Delivered,
                    DispatchOutcome::Delivered {
                        event: event.key.clone(),
                        receipt,
                    },
                )
            }
            Ok(_) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason: "agent receipt does not match the message".to_owned(),
                },
            ),
            Err(MessageSendError::Unavailable) if !attempted => (
                DeliveryOutcome::Pending,
                DispatchOutcome::Unavailable {
                    event: event.key.clone(),
                },
            ),
            Err(MessageSendError::Unavailable) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason: "agent disconnected after the submission attempt".to_owned(),
                },
            ),
            Err(
                MessageSendError::Uncertain(reason) | MessageSendError::NeedsReplacement(reason),
            ) if !attempted => (
                DeliveryOutcome::Failed,
                DispatchOutcome::Failed {
                    event: event.key.clone(),
                    reason,
                },
            ),
            Err(
                MessageSendError::Uncertain(reason) | MessageSendError::NeedsReplacement(reason),
            ) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason,
                },
            ),
        };
        let chat_name = match &outcome {
            DispatchOutcome::Delivered { receipt, .. } => receipt.chat_name.as_deref(),
            _ => None,
        };
        if !self.finish_delivery(&event.key, state, chat_name).await? {
            return Err(StoreError::DeliveryChanged);
        }
        match &outcome {
            DispatchOutcome::Delivered { receipt, .. } => {
                tracing::info!(native_message_id = %receipt.native_message_id, "inbound message delivered");
            }
            DispatchOutcome::Unavailable { .. } => {
                tracing::debug!("inbound message deferred because the agent is unavailable");
            }
            DispatchOutcome::Uncertain { reason, .. } => {
                tracing::warn!(%reason, "inbound submission could not be confirmed");
            }
            DispatchOutcome::Failed { reason, .. } => {
                tracing::warn!(%reason, "inbound request failed before submission");
            }
            DispatchOutcome::Idle => {}
        }
        Ok(outcome)
    }
}
