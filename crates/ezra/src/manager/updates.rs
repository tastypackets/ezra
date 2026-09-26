use std::time::Duration;

use tokio::time::MissedTickBehavior;

use super::agents::{Agent, ReleaseChannel};
use super::state::AppState;

const CHECK_INTERVAL: Duration = Duration::from_hours(6);

/// The newest release seen on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub channel: ReleaseChannel,
    pub version: String,
}

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
            self.check_for_update(agent).await;
        }
    }

    pub async fn check_for_update(&self, agent: Agent) {
        let Some(installed) = self.install_paths.installed_version(agent) else {
            return;
        };
        let channel = self.settings.lock().await.release_channel(agent);
        match agent
            .latest_version(channel, self.download_tls_verification)
            .await
        {
            Ok(version) => {
                if self.install_paths.installed_version(agent) == Some(installed) {
                    self.latest_releases
                        .lock()
                        .await
                        .insert(agent, LatestRelease { channel, version });
                }
            }
            Err(error) => tracing::warn!("could not check for a newer {agent}: {error}"),
        }
    }

    /// Checks Claude's new release channel once any running install is done.
    pub fn recheck_claude_release(&self) {
        let checking = self.clone();
        tokio::spawn(async move {
            drop(checking.install_lock.lock().await);
            checking.check_for_update(Agent::Claude).await;
        });
    }
}
