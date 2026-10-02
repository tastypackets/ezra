use super::*;
use crate::inbound::InboundEvent;
use crate::inbound::store::{DeliveryOutcome, TEST_QUEUE_LIMITS};

struct Fixture {
    directory: tempfile::TempDir,
    store: EventStore,
    target: SessionTarget,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary database");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        Self {
            directory,
            store,
            target: SessionTarget {
                host_id: "host-a".into(),
                agent: "codex".into(),
                chat_id: "chat-a".into(),
                workspace: "/repo".into(),
            },
        }
    }

    fn event(subject: &str, id: &str, new_chat: bool) -> InboundEvent {
        InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "test".into(),
                    subject: subject.into(),
                },
                id: id.into(),
            },
            new_chat,
            options: Default::default(),
            chat_name: None,
            source_url: None,
            initial_context: None,
            actor: "operator".into(),
            created_at: time::OffsetDateTime::now_utc(),
            message: "Request".into(),
        }
    }

    fn scope(&self) -> DeliveryScope<'_> {
        DeliveryScope {
            host_id: &self.target.host_id,
            agent: &self.target.agent,
        }
    }
}

#[tokio::test]
async fn multiple_links_reuse_one_distinct_session_and_existing_routes_stay_sticky() {
    let fixture = Fixture::new().await;
    let issue = Fixture::event("issue", "1", false);
    let other_issue = Fixture::event("other-issue", "2", false);
    let request = Fixture::event("pull", "3", false);
    fixture
        .store
        .bind_conversation(&issue.key.conversation, &fixture.target)
        .await
        .expect("issue binds");
    fixture
        .store
        .link_conversation(&other_issue.key.conversation, &issue.key.conversation)
        .await
        .expect("second issue shares chat");
    fixture
        .store
        .insert(&request, TEST_QUEUE_LIMITS)
        .await
        .expect("request inserts");
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &request.key,
                &[issue.key.conversation, other_issue.key.conversation],
                fixture.scope(),
                "/another-repository"
            )
            .await
            .expect("routing succeeds"),
        RoutingOutcome::Ready
    );
    assert_eq!(
        fixture
            .store
            .find_binding(&request.key.conversation)
            .await
            .expect("route reads"),
        Some(fixture.target)
    );
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &request.key,
                &[],
                DeliveryScope {
                    host_id: "host-a",
                    agent: "codex"
                },
                "/changed"
            )
            .await
            .expect("sticky route"),
        RoutingOutcome::Ready
    );
}

#[tokio::test]
async fn conflicting_links_create_separately_and_a_unique_foreign_route_waits() {
    let fixture = Fixture::new().await;
    let issue = Fixture::event("issue", "1", false);
    let foreign = Fixture::event("foreign", "2", false);
    fixture
        .store
        .bind_conversation(&issue.key.conversation, &fixture.target)
        .await
        .expect("issue binds");
    let foreign_target = SessionTarget {
        host_id: "host-b".into(),
        chat_id: "chat-b".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .bind_conversation(&foreign.key.conversation, &foreign_target)
        .await
        .expect("foreign issue binds");
    let request = Fixture::event("pull", "3", false);
    fixture
        .store
        .insert(&request, TEST_QUEUE_LIMITS)
        .await
        .expect("request inserts");
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &request.key,
                std::slice::from_ref(&foreign.key.conversation),
                fixture.scope(),
                "/repo"
            )
            .await
            .expect("foreign waits"),
        RoutingOutcome::Deferred
    );
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &request.key,
                &[issue.key.conversation, foreign.key.conversation],
                fixture.scope(),
                "/repo"
            )
            .await
            .expect("ambiguous routing"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
}

#[tokio::test]
async fn simultaneous_linked_requests_grant_only_one_native_creation() {
    let fixture = Fixture::new().await;
    let second_store = EventStore::open(&fixture.directory.path().join("ezra.db"))
        .await
        .expect("second connection opens");
    let issue = Fixture::event("issue", "1", false);
    let pull = Fixture::event("pull", "2", false);
    for event in [&issue, &pull] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    let issue_links = [pull.key.conversation.clone()];
    let pull_links = [issue.key.conversation.clone()];
    let (issue_claim, pull_claim) = tokio::join!(
        fixture
            .store
            .claim_routing(&issue.key, &issue_links, fixture.scope(), "/repo"),
        second_store.claim_routing(&pull.key, &pull_links, fixture.scope(), "/repo"),
    );
    let outcomes = [
        issue_claim.expect("issue claim"),
        pull_claim.expect("pull claim"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RoutingOutcome::Create { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == RoutingOutcome::Deferred)
            .count(),
        1
    );
}

#[tokio::test]
async fn reset_uses_incoming_checkout_after_delivery_and_detaches_only_one_alias_across_restart() {
    let fixture = Fixture::new().await;
    let issue = Fixture::event("issue", "1", false);
    let reset = Fixture::event("pull", "2", true);
    fixture
        .store
        .bind_conversation(&issue.key.conversation, &fixture.target)
        .await
        .expect("issue binds");
    fixture
        .store
        .link_conversation(&reset.key.conversation, &issue.key.conversation)
        .await
        .expect("pull shares chat");
    for event in [&issue, &reset] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    fixture
        .store
        .claim_next(Some(fixture.scope()))
        .await
        .expect("issue claims")
        .expect("issue request");
    assert_eq!(
        fixture
            .store
            .claim_routing(&reset.key, &[], fixture.scope(), "/changed")
            .await
            .expect("reset waits"),
        RoutingOutcome::Deferred
    );
    fixture
        .store
        .replace_chat_for_delivery(&issue.key, &fixture.target, "recovered-chat")
        .await
        .expect("deleted-chat recovery still works");
    fixture
        .store
        .finish_delivery(&issue.key, DeliveryOutcome::Uncertain, None)
        .await
        .expect("issue becomes uncertain");
    assert_eq!(
        fixture
            .store
            .claim_routing(&reset.key, &[], fixture.scope(), "/changed")
            .await
            .expect("uncertain other alias permits escape"),
        RoutingOutcome::Create {
            workspace: "/changed".into()
        }
    );
    let fresh_target = SessionTarget {
        chat_id: "fresh-chat".into(),
        workspace: "/changed".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .finish_routing(&reset.key, &fresh_target)
        .await
        .expect("reset commits");
    fixture.store.pool.close().await;
    let reopened = EventStore::open(&fixture.directory.path().join("ezra.db"))
        .await
        .expect("store reopens");
    assert_eq!(
        reopened
            .find_binding(&reset.key.conversation)
            .await
            .expect("reset route"),
        Some(fresh_target)
    );
    assert_eq!(
        reopened
            .find_binding(&issue.key.conversation)
            .await
            .expect("other route")
            .expect("other binding")
            .chat_id,
        "recovered-chat"
    );
    assert_eq!(
        reopened
            .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
            .await
            .expect("reset retry"),
        RoutingOutcome::Ready
    );
    assert_eq!(
        reopened
            .claim_next(Some(fixture.scope()))
            .await
            .expect("fresh delivery proceeds"),
        Some(reset)
    );
}

#[tokio::test]
async fn an_interrupted_unattempted_reset_can_prepare_again_without_confirming_a_message() {
    let fixture = Fixture::new().await;
    let reset = Fixture::event("issue", "1", true);
    fixture
        .store
        .bind_conversation(&reset.key.conversation, &fixture.target)
        .await
        .expect("old chat binds");
    fixture
        .store
        .insert(&reset, TEST_QUEUE_LIMITS)
        .await
        .expect("reset inserts");
    fixture
        .store
        .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
        .await
        .expect("reset claims");
    fixture
        .store
        .recover_interrupted()
        .await
        .expect("interruption recovers");
    assert_eq!(
        fixture
            .store
            .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
            .await
            .expect("unattempted reset prepares again"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
    assert!(
        !fixture
            .store
            .confirm_delivery(&reset.key)
            .await
            .expect("unapplied reset cannot confirm")
    );
    assert_eq!(
        fixture
            .store
            .find_binding(&reset.key.conversation)
            .await
            .expect("old route remains"),
        Some(fixture.target)
    );
}

#[tokio::test]
async fn resetting_the_last_alias_removes_only_unreferenced_session_metadata() {
    let fixture = Fixture::new().await;
    let reset = Fixture::event("issue", "1", true);
    fixture
        .store
        .bind_conversation(&reset.key.conversation, &fixture.target)
        .await
        .expect("old chat binds");
    fixture
        .store
        .insert(&reset, TEST_QUEUE_LIMITS)
        .await
        .expect("reset inserts");
    fixture
        .store
        .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
        .await
        .expect("reset claims");
    let fresh_target = SessionTarget {
        chat_id: "fresh-chat".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .finish_routing(&reset.key, &fresh_target)
        .await
        .expect("reset commits");
    let remaining = sqlx::query_scalar!("SELECT COUNT(*) FROM inbound_sessions")
        .fetch_one(&fixture.store.pool)
        .await
        .expect("sessions counted");
    assert_eq!(remaining, 1);
    assert_eq!(
        fixture
            .store
            .find_binding(&reset.key.conversation)
            .await
            .expect("fresh binding"),
        Some(fresh_target)
    );
}

#[tokio::test]
async fn a_failed_routing_commit_keeps_the_original_alias() {
    let fixture = Fixture::new().await;
    let reset = Fixture::event("issue", "1", true);
    fixture
        .store
        .bind_conversation(&reset.key.conversation, &fixture.target)
        .await
        .expect("old chat binds");
    fixture
        .store
        .insert(&reset, TEST_QUEUE_LIMITS)
        .await
        .expect("reset inserts");
    fixture
        .store
        .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
        .await
        .expect("reset claims");
    assert!(
        fixture
            .store
            .finish_routing(&reset.key, &fixture.target)
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .find_binding(&reset.key.conversation)
            .await
            .expect("old route remains"),
        Some(fixture.target)
    );
    assert_eq!(
        fixture
            .store
            .delivery_state(&reset.key)
            .await
            .expect("claim remains"),
        Some(DeliveryState::Delivering)
    );
}

#[tokio::test]
async fn durable_routing_retries_include_only_unrouted_requests_in_the_selected_scope() {
    let fixture = Fixture::new().await;
    let unbound = Fixture::event("repo/unbound", "1", false);
    let ordinary = Fixture::event("repo/bound", "2", false);
    let reset = Fixture::event("repo/bound", "3", true);
    let other_repo = Fixture::event("other/unbound", "4", false);
    fixture
        .store
        .bind_conversation(&ordinary.key.conversation, &fixture.target)
        .await
        .expect("discussion binds");
    for event in [&unbound, &ordinary, &reset, &other_repo] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    assert_eq!(
        fixture
            .store
            .pending_routing("test", "repo/")
            .await
            .expect("durable retry list"),
        vec![unbound, reset]
    );
    assert!(
        fixture
            .store
            .pending_routing("other-source", "repo/")
            .await
            .expect("other source")
            .is_empty()
    );
}

#[tokio::test]
async fn an_older_same_discussion_uncertainty_does_not_block_a_new_reset() {
    let fixture = Fixture::new().await;
    let older = Fixture::event("issue", "1", false);
    let reset = Fixture::event("issue", "2", true);
    fixture
        .store
        .bind_conversation(&older.key.conversation, &fixture.target)
        .await
        .expect("discussion binds");
    for event in [&older, &reset] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    assert_eq!(
        fixture
            .store
            .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
            .await
            .expect("pending older blocks"),
        RoutingOutcome::Deferred
    );
    fixture
        .store
        .claim_next(Some(fixture.scope()))
        .await
        .expect("older claims");
    fixture
        .store
        .finish_delivery(&older.key, DeliveryOutcome::Uncertain, None)
        .await
        .expect("older uncertain");
    assert_eq!(
        fixture
            .store
            .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
            .await
            .expect("uncertain older no longer blocks"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
    assert_eq!(
        fixture
            .store
            .delivery_state(&older.key)
            .await
            .expect("older audit"),
        Some(DeliveryState::Uncertain)
    );
}

#[tokio::test]
async fn routing_waits_for_all_related_creations_before_counting_known_sessions() {
    let fixture = Fixture::new().await;
    let known = Fixture::event("known", "1", false);
    let creating = Fixture::event("creating", "2", false);
    let request = Fixture::event("pull", "3", false);
    fixture
        .store
        .bind_conversation(&known.key.conversation, &fixture.target)
        .await
        .expect("known issue binds");
    for event in [&creating, &request] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    fixture
        .store
        .claim_routing(&creating.key, &[], fixture.scope(), "/repo")
        .await
        .expect("other creation claims");
    let related = [known.key.conversation, creating.key.conversation.clone()];
    assert_eq!(
        fixture
            .store
            .claim_routing(&request.key, &related, fixture.scope(), "/repo")
            .await
            .expect("partial candidate set waits"),
        RoutingOutcome::Deferred
    );
    let second = SessionTarget {
        chat_id: "second-chat".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .finish_routing(&creating.key, &second)
        .await
        .expect("other creation finishes");
    assert_eq!(
        fixture
            .store
            .claim_routing(&request.key, &related, fixture.scope(), "/repo")
            .await
            .expect("complete candidate set is ambiguous"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
}

#[tokio::test]
async fn a_new_alias_waits_for_a_related_reset_and_then_uses_its_fresh_route() {
    let fixture = Fixture::new().await;
    let reset = Fixture::event("issue", "1", true);
    let request = Fixture::event("pull", "2", false);
    fixture
        .store
        .bind_conversation(&reset.key.conversation, &fixture.target)
        .await
        .expect("old issue chat binds");
    for event in [&reset, &request] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    let related = std::slice::from_ref(&reset.key.conversation);
    assert_eq!(
        fixture
            .store
            .claim_routing(&request.key, related, fixture.scope(), "/repo")
            .await
            .expect("pending reset waits"),
        RoutingOutcome::Deferred
    );
    fixture
        .store
        .claim_routing(&reset.key, &[], fixture.scope(), "/repo")
        .await
        .expect("reset claims");
    let fresh = SessionTarget {
        chat_id: "fresh-chat".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .finish_routing(&reset.key, &fresh)
        .await
        .expect("reset finishes");
    assert_eq!(
        fixture
            .store
            .claim_routing(&request.key, related, fixture.scope(), "/repo")
            .await
            .expect("new alias routes"),
        RoutingOutcome::Ready
    );
    assert_eq!(
        fixture
            .store
            .find_binding(&request.key.conversation)
            .await
            .expect("new alias binding"),
        Some(fresh)
    );
}

#[tokio::test]
async fn crossed_unbound_backlogs_with_later_resets_can_make_progress() {
    let fixture = Fixture::new().await;
    let first = Fixture::event("issue", "1", false);
    let second = Fixture::event("pull", "2", false);
    let first_reset = Fixture::event("issue", "3", true);
    let second_reset = Fixture::event("pull", "4", true);
    for event in [&first, &second, &first_reset, &second_reset] {
        fixture
            .store
            .insert(event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
    }
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &first.key,
                std::slice::from_ref(&second.key.conversation),
                fixture.scope(),
                "/repo"
            )
            .await
            .expect("older unrouted request can start"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
    fixture
        .store
        .finish_routing(&first.key, &fixture.target)
        .await
        .expect("first chat binds");
    fixture
        .store
        .claim_next(Some(fixture.scope()))
        .await
        .expect("first request claims");
    fixture
        .store
        .finish_delivery(&first.key, DeliveryOutcome::Delivered, None)
        .await
        .expect("first request delivered");
    assert_eq!(
        fixture
            .store
            .claim_routing(&first_reset.key, &[], fixture.scope(), "/repo")
            .await
            .expect("first reset can start"),
        RoutingOutcome::Create {
            workspace: "/repo".into()
        }
    );
    let fresh = SessionTarget {
        chat_id: "fresh-chat".into(),
        ..fixture.target.clone()
    };
    fixture
        .store
        .finish_routing(&first_reset.key, &fresh)
        .await
        .expect("first reset binds");
    assert_eq!(
        fixture
            .store
            .claim_routing(
                &second.key,
                std::slice::from_ref(&first.key.conversation),
                fixture.scope(),
                "/repo"
            )
            .await
            .expect("second backlog now routes"),
        RoutingOutcome::Ready
    );
}
