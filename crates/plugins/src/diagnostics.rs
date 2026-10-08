//! Where the runtime's own diagnostics go. The host and window runtimes report through `tracing`
//! (a spawn filter's late answer, a hook disabled for its budget, a plugin that failed to load,
//! §9.2, §9.7); [`to_stderr`] makes a process print them, so the daemon's log keeps them.

use std::fmt::Write as _;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber, span};

/// Prints `tracing` events at info level and above to stderr, one line each:
/// `<prefix>: <LEVEL> <message> <field>=<value>…`. Installs itself as the process's global
/// subscriber; returns false when another one was installed first.
pub fn to_stderr(prefix: &'static str) -> bool {
    tracing::subscriber::set_global_default(Stderr { prefix }).is_ok()
}

struct Stderr {
    prefix: &'static str,
}

/// The line an event prints, without the prefix.
fn line(level: &Level, event: &Event<'_>) -> String {
    let mut fields = Fields::default();
    event.record(&mut fields);
    format!("{level} {}{}", fields.message, fields.rest)
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl Subscriber for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::INFO
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::INFO)
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        eprintln!("{}: {}", self.prefix, line(event.metadata().level(), event));
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collects lines as [`Stderr`] would print them.
    struct Lines(Arc<Mutex<Vec<String>>>);

    impl Subscriber for Lines {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            Stderr { prefix: "" }.enabled(metadata)
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            self.0.lock().unwrap().push(line(event.metadata().level(), event));
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    #[test]
    fn events_print_their_level_message_and_fields() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(Lines(lines.clone()), || {
            tracing::warn!(plugin = %"slow", "plugin spawn filter took over 50 ms; skipped");
            tracing::info!(n = 3, "counted");
            tracing::debug!("too chatty");
        });
        assert_eq!(
            *lines.lock().unwrap(),
            [
                "WARN plugin spawn filter took over 50 ms; skipped plugin=slow",
                "INFO counted n=3",
            ]
        );
    }
}
