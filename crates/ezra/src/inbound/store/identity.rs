use super::{EventStore, StoreError};

impl EventStore {
    pub async fn host_id(&self) -> Result<String, StoreError> {
        let host_id =
            sqlx::query_scalar!("SELECT host_id FROM manager_identity WHERE singleton = 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(host_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::ConversationKey;
    use crate::inbound::store::SessionTarget;

    #[tokio::test]
    async fn host_identity_survives_reopening_and_preserves_session_ownership() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let host_id = store.host_id().await.expect("identity reads");
        assert_eq!(host_id.len(), 32);
        assert!(host_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let conversation = ConversationKey {
            source: "github:github.com".to_owned(),
            subject: "issue-1".to_owned(),
        };
        let target = SessionTarget {
            host_id: host_id.clone(),
            agent: "codex".to_owned(),
            chat_id: "chat-1".to_owned(),
            workspace: "/home/dev/projects/repository".to_owned(),
        };
        store
            .bind_conversation(&conversation, &target)
            .await
            .expect("session binds");
        store.pool.close().await;
        let store = EventStore::open(&path).await.expect("store reopens");
        assert_eq!(
            store.host_id().await.expect("identity reads again"),
            host_id
        );
        assert_eq!(
            store
                .find_binding(&conversation)
                .await
                .expect("session lookup"),
            Some(target)
        );
        let second_connection = EventStore::open(&path)
            .await
            .expect("second connection opens");
        assert_eq!(
            second_connection.host_id().await.expect("shared identity"),
            host_id
        );
    }

    #[tokio::test]
    async fn independent_databases_have_independent_identities() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = EventStore::open(&directory.path().join("first.db"))
            .await
            .expect("first store opens");
        let second = EventStore::open(&directory.path().join("second.db"))
            .await
            .expect("second store opens");
        assert_ne!(
            first.host_id().await.expect("first identity"),
            second.host_id().await.expect("second identity")
        );
    }

    #[tokio::test]
    async fn empty_database_receives_an_identity_on_startup() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("empty database opens");
        pool.close().await;
        let store = EventStore::open(&path).await.expect("database initializes");
        assert_eq!(store.host_id().await.expect("new identity reads").len(), 32);
    }
}
