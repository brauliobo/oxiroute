//! The process logger: the `RUST_LOG` filter until a configuration's `log_filter` replaces it.

use std::sync::{PoisonError, RwLock};

use env_logger::{Builder, Logger};
use log::{Log, Metadata, Record};

struct ProcessLogger(RwLock<Option<Logger>>);

static LOGGER: ProcessLogger = ProcessLogger(RwLock::new(None));

impl ProcessLogger {
    fn with<T>(&self, default: T, use_logger: impl FnOnce(&Logger) -> T) -> T {
        let logger = self.0.read().unwrap_or_else(PoisonError::into_inner);
        logger.as_ref().map_or(default, use_logger)
    }
}

impl Log for ProcessLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.with(false, |logger| logger.enabled(metadata))
    }

    fn log(&self, record: &Record) {
        self.with((), |logger| logger.log(record));
    }

    fn flush(&self) {
        self.with((), Log::flush);
    }
}

/// Installs the process logger with the `RUST_LOG` filter; call once at process start.
///
/// # Panics
///
/// Panics when another logger is already installed.
pub fn init_logging() {
    log::set_logger(&LOGGER).expect("no other logger is installed");
    apply_log_filter(None);
}

/// Replaces the log filter with a configuration's `log_filter`, or with `RUST_LOG` when it is `None`.
pub fn apply_log_filter(filter: Option<&str>) {
    let logger = match filter {
        Some(filter) => Builder::new().parse_filters(filter).build(),
        None => Builder::from_default_env().build(),
    };
    log::set_max_level(logger.filter());
    *LOGGER.0.write().unwrap_or_else(PoisonError::into_inner) = Some(logger);
}

#[cfg(test)]
mod tests {
    use log::{Level, LevelFilter};

    use super::*;

    fn enabled(logger: &Logger, target: &str, level: Level) -> bool {
        logger.enabled(&Metadata::builder().target(target).level(level).build())
    }

    #[test]
    fn a_configured_filter_applies_levels_and_module_directives() {
        let logger = Builder::new()
            .parse_filters("warn,oxiroute::audit=debug")
            .build();

        assert_eq!(logger.filter(), LevelFilter::Debug);
        assert!(enabled(&logger, "oxiroute::audit", Level::Debug));
        assert!(!enabled(&logger, "oxiroute::http", Level::Info));
        assert!(enabled(&logger, "oxiroute::http", Level::Warn));
    }

    /// Owns the process-wide logger: it is the only test that installs one.
    #[test]
    fn applying_a_filter_replaces_it_and_none_restores_the_environment_filter() {
        init_logging();
        let environment = Builder::from_default_env().build().filter();

        apply_log_filter(Some("trace"));
        assert_eq!(log::max_level(), LevelFilter::Trace);
        assert!(LOGGER.enabled(&Metadata::builder().level(Level::Trace).build()));

        apply_log_filter(Some("off"));
        assert_eq!(log::max_level(), LevelFilter::Off);
        assert!(!LOGGER.enabled(&Metadata::builder().level(Level::Error).build()));

        apply_log_filter(None);
        assert_eq!(log::max_level(), environment);
    }
}
