use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

pub struct Logging {
    filter: EnvFilter,
}

impl Logging {
    pub fn new(directives: &str) -> Self {
        Self {
            filter: EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .parse_lossy(directives),
        }
    }

    pub fn init(self) {
        tracing_subscriber::fmt()
            .with_env_filter(self.filter)
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
            .with_target(false)
            .init();
    }
}

/// What this thread logs while a capture runs, formatted as the manager writes it without the
/// time.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer locks")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The process-wide subscriber while captures run. It records nothing.
#[cfg(test)]
struct Quiet;

// A callsite first hit while only one subscriber exists caches that subscriber's interest for
// every thread, so a thread without a capture could switch a callsite off for the captures.
#[cfg(test)]
impl tracing::Subscriber for Quiet {
    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        false
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[cfg(test)]
impl CapturedLogs {
    /// Captures what this thread logs at `directives` until the guard is dropped.
    pub fn start(directives: &str) -> (Self, tracing::subscriber::DefaultGuard) {
        static QUIET: std::sync::Once = std::sync::Once::new();
        QUIET.call_once(|| {
            tracing::subscriber::set_global_default(Quiet)
                .expect("no other test sets a global subscriber");
        });
        let logs = Self::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(Logging::new(directives).filter)
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(move || writer.clone())
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("log buffer locks").clone())
            .expect("formatted logs are UTF-8")
    }

    /// The captured lines that contain `text`.
    pub fn lines_with(&self, text: &str) -> Vec<String> {
        self.text()
            .lines()
            .filter(|line| line.contains(text))
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl CapturedLogs {
        fn markers(directives: &str) -> String {
            let (logs, capture) = Self::start(directives);
            tracing::trace!(target: "ezra::inbound", "trace-marker");
            tracing::debug!(target: "ezra::inbound", "debug-marker");
            tracing::info!(target: "ezra::inbound", "info-marker");
            tracing::warn!(target: "ezra::inbound", "warn-marker");
            tracing::error!(target: "ezra::inbound", "error-marker");
            tracing::info!(target: "dependency", "dependency-marker");
            drop(capture);
            logs.text()
        }
    }

    #[test]
    fn default_filter_keeps_info_and_above() {
        let output = CapturedLogs::markers("");
        assert!(!output.contains("trace-marker"));
        assert!(!output.contains("debug-marker"));
        for message in ["info-marker", "warn-marker", "error-marker"] {
            assert!(output.contains(message), "{output}");
        }
    }

    #[test]
    fn warning_and_off_filters_reduce_noise() {
        let output = CapturedLogs::markers("warn");
        for message in [
            "trace-marker",
            "debug-marker",
            "info-marker",
            "dependency-marker",
        ] {
            assert!(!output.contains(message), "{output}");
        }
        assert!(output.contains("warn-marker"));
        assert!(output.contains("error-marker"));
        assert!(CapturedLogs::markers("off").is_empty());
    }

    #[test]
    fn targeted_debug_does_not_enable_dependency_noise() {
        let output = CapturedLogs::markers("warn,ezra::inbound=debug");
        assert!(output.contains("debug-marker"));
        assert!(output.contains("info-marker"));
        assert!(!output.contains("trace-marker"));
        assert!(!output.contains("dependency-marker"));
    }
}
