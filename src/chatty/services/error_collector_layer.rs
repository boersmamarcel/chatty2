use crate::chatty::models::error_store::{ErrorEntry, ErrorLevel};
use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::SystemTime;
use tracing::{
    Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::Layer;

/// Visitor to extract fields from tracing events
struct FieldVisitor {
    message: Option<String>,
    fields: HashMap<String, String>,
}

impl FieldVisitor {
    fn new() -> Self {
        Self {
            message: None,
            fields: HashMap::new(),
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let value_str = format!("{:?}", value);

        if field.name() == "message" {
            self.message = Some(value_str);
        } else {
            self.fields.insert(field.name().to_string(), value_str);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }
}

/// Custom tracing layer that collects WARN and ERROR level events
pub struct ErrorCollectorLayer {
    sender: SyncSender<ErrorEntry>,
}

impl ErrorCollectorLayer {
    pub fn new() -> (Self, Receiver<ErrorEntry>) {
        let (tx, rx) = sync_channel(1000); // Bounded to prevent memory exhaustion
        (Self { sender: tx }, rx)
    }
}

impl<S> Layer<S> for ErrorCollectorLayer
where
    S: Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();

        // Only capture WARN and ERROR levels
        if !matches!(*metadata.level(), Level::WARN | Level::ERROR) {
            return;
        }

        // Extract fields using visitor pattern
        let mut visitor = FieldVisitor::new();
        event.record(&mut visitor);

        let entry = ErrorEntry {
            timestamp: SystemTime::now(),
            level: if *metadata.level() == Level::ERROR {
                ErrorLevel::Error
            } else {
                ErrorLevel::Warning
            },
            message: visitor.message.unwrap_or_default(),
            target: metadata.target().to_string(),
            file: metadata.file().map(String::from),
            line: metadata.line(),
            fields: visitor.fields,
        };

        // Non-blocking send - drop if channel full (prevents backpressure)
        let _ = self.sender.try_send(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// Helper: create a subscriber with ErrorCollectorLayer and return the receiver
    fn setup_collector() -> (impl tracing::Subscriber, Receiver<ErrorEntry>) {
        let (layer, rx) = ErrorCollectorLayer::new();
        let subscriber = tracing_subscriber::registry().with(layer);
        (subscriber, rx)
    }

    #[test]
    fn test_captures_error_events() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("something failed");
        });

        let entry = rx.try_recv().expect("should receive an error entry");
        assert_eq!(entry.level, ErrorLevel::Error);
        assert!(
            entry.message.contains("something failed"),
            "message was: {}",
            entry.message
        );
    }

    #[test]
    fn test_captures_warn_events() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("careful now");
        });

        let entry = rx.try_recv().expect("should receive a warning entry");
        assert_eq!(entry.level, ErrorLevel::Warning);
        assert!(
            entry.message.contains("careful now"),
            "message was: {}",
            entry.message
        );
    }

    #[test]
    fn test_ignores_info_events() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("just info");
        });

        assert!(rx.try_recv().is_err(), "should not receive any entry");
    }

    #[test]
    fn test_ignores_debug_events() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("debug stuff");
        });

        assert!(rx.try_recv().is_err(), "should not receive any entry");
    }

    #[test]
    fn test_ignores_trace_events() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!("trace stuff");
        });

        assert!(rx.try_recv().is_err(), "should not receive any entry");
    }

    #[test]
    fn test_captures_target() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "my::module", "targeted error");
        });

        let entry = rx.try_recv().expect("should receive entry");
        assert_eq!(entry.target, "my::module");
    }

    #[test]
    fn test_captures_extra_fields() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(user_id = 42, action = "login", "auth failed");
        });

        let entry = rx.try_recv().expect("should receive entry");
        assert!(entry.fields.contains_key("user_id"));
        assert!(entry.fields.contains_key("action"));
    }

    #[test]
    fn test_multiple_events_in_order() {
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("first error");
            tracing::warn!("first warning");
            tracing::error!("second error");
        });

        let e1 = rx.try_recv().expect("first");
        let e2 = rx.try_recv().expect("second");
        let e3 = rx.try_recv().expect("third");

        assert_eq!(e1.level, ErrorLevel::Error);
        assert!(e1.message.contains("first error"));
        assert_eq!(e2.level, ErrorLevel::Warning);
        assert!(e2.message.contains("first warning"));
        assert_eq!(e3.level, ErrorLevel::Error);
        assert!(e3.message.contains("second error"));
    }

    #[test]
    fn test_timestamp_is_recent() {
        let before = SystemTime::now();
        let (subscriber, rx) = setup_collector();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("timed event");
        });
        let after = SystemTime::now();

        let entry = rx.try_recv().expect("should receive entry");
        assert!(entry.timestamp >= before);
        assert!(entry.timestamp <= after);
    }

    #[test]
    fn test_channel_bounded_does_not_panic() {
        // Create a layer with a small channel and overflow it
        let (tx, _rx) = sync_channel(2);
        let layer = ErrorCollectorLayer { sender: tx };
        let subscriber = tracing_subscriber::registry().with(layer);

        // Send more events than the channel can hold; should not panic
        tracing::subscriber::with_default(subscriber, || {
            for i in 0..10 {
                tracing::error!("overflow event {}", i);
            }
        });

        // If we get here, no panic occurred - the try_send gracefully drops
    }
}
