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
