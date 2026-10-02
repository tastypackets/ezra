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
