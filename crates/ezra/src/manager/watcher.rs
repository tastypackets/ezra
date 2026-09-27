use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};

/// Watches directories, not what is below them, and tells when the events that matter settle.
/// The default watches nothing and never tells.
#[derive(Default)]
pub struct SettledWatcher {
    watcher: Option<RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    changed: Arc<Notify>,
}

impl SettledWatcher {
    /// Watches `always` for good, noticing the events `relevant` accepts and any watcher error.
    pub fn start(
        always: &[&Path],
        relevant: impl Fn(&notify::Event) -> bool + Send + 'static,
    ) -> notify::Result<Self> {
        let changed = Arc::new(Notify::new());
        let notifier = Arc::clone(&changed);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.map_or(true, |event| relevant(&event)) {
                    notifier.notify_one();
                }
            })?;
        for path in always {
            watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        Ok(Self {
            watcher: Some(watcher),
            watched: BTreeSet::new(),
            changed,
        })
    }

    pub fn is_watching(&self) -> bool {
        self.watcher.is_some()
    }

    /// Watches each of `paths` and stops watching the ones earlier calls gave that `paths` leaves
    /// out. A path that was removed and made again is watched again.
    pub fn watch_only(&mut self, paths: BTreeSet<PathBuf>) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        self.watched.retain(|path| {
            let keep = paths.contains(path);
            if !keep {
                let _already_gone = watcher.unwatch(path);
            }
            keep
        });
        for path in paths {
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watched.insert(path);
            }
        }
    }

    /// Returns after a relevant event once `quiet` passes without another, or after `longest` of
    /// events.
    pub async fn settled_change(&self, quiet: Duration, longest: Duration) {
        self.changed.notified().await;
        let Some(deadline) = Instant::now().checked_add(longest) else {
            return;
        };
        while Instant::now() < deadline && timeout(quiet, self.changed.notified()).await.is_ok() {}
    }
}
