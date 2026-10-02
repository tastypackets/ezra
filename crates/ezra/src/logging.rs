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

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("log buffer locks")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Logs {
        fn capture(directives: &str) -> String {
            let logs = Self::default();
            let writer = logs.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(Logging::new(directives).filter)
                .with_ansi(false)
                .without_time()
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                tracing::trace!(target: "ezra::inbound", "trace-marker");
                tracing::debug!(target: "ezra::inbound", "debug-marker");
                tracing::info!(target: "ezra::inbound", "info-marker");
                tracing::warn!(target: "ezra::inbound", "warn-marker");
                tracing::error!(target: "ezra::inbound", "error-marker");
                tracing::info!(target: "dependency", "dependency-marker");
            });
            String::from_utf8(logs.0.lock().expect("log buffer locks").clone())
                .expect("formatted logs are UTF-8")
        }
    }

    #[test]
    fn default_filter_keeps_info_and_above() {
        let output = Logs::capture("");
        assert!(!output.contains("trace-marker"));
        assert!(!output.contains("debug-marker"));
        for message in ["info-marker", "warn-marker", "error-marker"] {
            assert!(output.contains(message), "{output}");
        }
    }

    #[test]
    fn warning_and_off_filters_reduce_noise() {
        let output = Logs::capture("warn");
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
        assert!(Logs::capture("off").is_empty());
    }

    #[test]
    fn targeted_debug_does_not_enable_dependency_noise() {
        let output = Logs::capture("warn,ezra::inbound=debug");
        assert!(output.contains("debug-marker"));
        assert!(output.contains("info-marker"));
        assert!(!output.contains("trace-marker"));
        assert!(!output.contains("dependency-marker"));
    }
}
