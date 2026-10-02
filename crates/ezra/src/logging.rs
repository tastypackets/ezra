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
