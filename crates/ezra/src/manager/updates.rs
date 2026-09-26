use std::time::Duration;

use tokio::time::MissedTickBehavior;

use super::agents::Agent;
use super::state::AppState;

const CHECK_INTERVAL: Duration = Duration::from_hours(6);

impl AppState {
    pub async fn check_for_updates_regularly(self) {
        let mut interval = tokio::time::interval(CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.check_for_updates().await;
        }
    }

    async fn check_for_updates(&self) {
        for agent in Agent::ALL {
            let Some(installed) = self.install_paths.installed_version(agent) else {
                continue;
            };
            match agent.latest_version(self.download_tls_verification).await {
                Ok(version) => {
                    if self.install_paths.installed_version(agent) == Some(installed) {
                        self.latest_versions.lock().await.insert(agent, version);
                    }
                }
                Err(error) => tracing::warn!("could not check for a newer {agent}: {error}"),
            }
        }
    }
}
