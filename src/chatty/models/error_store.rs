use gpui::Global;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn format_timestamp(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;
            format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
        }
        Err(_) => "Unknown time".to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrorLevel {
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct ErrorEntry {
    pub timestamp: SystemTime,
    pub level: ErrorLevel,
    pub message: String,
    pub target: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub fields: HashMap<String, String>,
}

pub struct ErrorStore {
    entries: Arc<Mutex<Vec<ErrorEntry>>>,
    max_entries: usize,
}

impl Global for ErrorStore {}

impl ErrorStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            max_entries,
        }
    }

    pub fn add_entry(&self, entry: ErrorEntry) {
        let mut entries = self.entries.lock().unwrap();
        entries.push(entry);

        // FIFO eviction when exceeding max
        if entries.len() > self.max_entries {
            entries.remove(0);
        }
    }

    pub fn get_all_entries(&self) -> Vec<ErrorEntry> {
        let entries = self.entries.lock().unwrap();
        entries.clone()
    }

    pub fn error_count(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| e.level == ErrorLevel::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| e.level == ErrorLevel::Warning)
            .count()
    }

    pub fn clear(&mut self) {
        let mut entries = self.entries.lock().unwrap();
        entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(level: ErrorLevel, message: &str) -> ErrorEntry {
        ErrorEntry {
            timestamp: SystemTime::now(),
            level,
            message: message.to_string(),
            target: "test::target".to_string(),
            file: None,
            line: None,
            fields: HashMap::new(),
        }
    }

    #[test]
    fn test_new_store_is_empty() {
        let store = ErrorStore::new(10);
        assert_eq!(store.get_all_entries().len(), 0);
        assert_eq!(store.error_count(), 0);
        assert_eq!(store.warning_count(), 0);
    }

    #[test]
    fn test_add_single_error() {
        let store = ErrorStore::new(10);
        store.add_entry(make_entry(ErrorLevel::Error, "something broke"));

        assert_eq!(store.get_all_entries().len(), 1);
        assert_eq!(store.error_count(), 1);
        assert_eq!(store.warning_count(), 0);
        assert_eq!(store.get_all_entries()[0].message, "something broke");
    }

    #[test]
    fn test_add_single_warning() {
        let store = ErrorStore::new(10);
        store.add_entry(make_entry(ErrorLevel::Warning, "heads up"));

        assert_eq!(store.get_all_entries().len(), 1);
        assert_eq!(store.error_count(), 0);
        assert_eq!(store.warning_count(), 1);
    }

    #[test]
    fn test_mixed_errors_and_warnings() {
        let store = ErrorStore::new(10);
        store.add_entry(make_entry(ErrorLevel::Error, "err1"));
        store.add_entry(make_entry(ErrorLevel::Warning, "warn1"));
        store.add_entry(make_entry(ErrorLevel::Error, "err2"));
        store.add_entry(make_entry(ErrorLevel::Warning, "warn2"));
        store.add_entry(make_entry(ErrorLevel::Warning, "warn3"));

        assert_eq!(store.get_all_entries().len(), 5);
        assert_eq!(store.error_count(), 2);
        assert_eq!(store.warning_count(), 3);
    }

    #[test]
    fn test_fifo_eviction_at_max_capacity() {
        let store = ErrorStore::new(3);
        store.add_entry(make_entry(ErrorLevel::Error, "first"));
        store.add_entry(make_entry(ErrorLevel::Error, "second"));
        store.add_entry(make_entry(ErrorLevel::Error, "third"));

        // At capacity, no eviction yet
        assert_eq!(store.get_all_entries().len(), 3);
        assert_eq!(store.get_all_entries()[0].message, "first");

        // Adding a 4th should evict the first
        store.add_entry(make_entry(ErrorLevel::Error, "fourth"));
        assert_eq!(store.get_all_entries().len(), 3);
        assert_eq!(store.get_all_entries()[0].message, "second");
        assert_eq!(store.get_all_entries()[2].message, "fourth");
    }

    #[test]
    fn test_fifo_eviction_preserves_order() {
        let store = ErrorStore::new(2);
        store.add_entry(make_entry(ErrorLevel::Warning, "a"));
        store.add_entry(make_entry(ErrorLevel::Error, "b"));
        store.add_entry(make_entry(ErrorLevel::Warning, "c"));
        store.add_entry(make_entry(ErrorLevel::Error, "d"));

        let entries = store.get_all_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "c");
        assert_eq!(entries[1].message, "d");
    }

    #[test]
    fn test_clear() {
        let mut store = ErrorStore::new(10);
        store.add_entry(make_entry(ErrorLevel::Error, "err"));
        store.add_entry(make_entry(ErrorLevel::Warning, "warn"));
        assert_eq!(store.get_all_entries().len(), 2);

        store.clear();
        assert_eq!(store.get_all_entries().len(), 0);
        assert_eq!(store.error_count(), 0);
        assert_eq!(store.warning_count(), 0);
    }

    #[test]
    fn test_entry_fields_preserved() {
        let store = ErrorStore::new(10);
        let mut fields = HashMap::new();
        fields.insert("key1".to_string(), "value1".to_string());
        fields.insert("key2".to_string(), "value2".to_string());

        let entry = ErrorEntry {
            timestamp: SystemTime::now(),
            level: ErrorLevel::Error,
            message: "with fields".to_string(),
            target: "my::module".to_string(),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            fields,
        };

        store.add_entry(entry);

        let entries = store.get_all_entries();
        assert_eq!(entries[0].target, "my::module");
        assert_eq!(entries[0].file, Some("src/main.rs".to_string()));
        assert_eq!(entries[0].line, Some(42));
        assert_eq!(entries[0].fields.get("key1").unwrap(), "value1");
        assert_eq!(entries[0].fields.get("key2").unwrap(), "value2");
    }

    #[test]
    fn test_max_entries_of_one() {
        let store = ErrorStore::new(1);
        store.add_entry(make_entry(ErrorLevel::Error, "first"));
        assert_eq!(store.get_all_entries().len(), 1);

        store.add_entry(make_entry(ErrorLevel::Warning, "second"));
        assert_eq!(store.get_all_entries().len(), 1);
        assert_eq!(store.get_all_entries()[0].message, "second");
        assert_eq!(store.error_count(), 0);
        assert_eq!(store.warning_count(), 1);
    }

    #[test]
    fn test_counts_after_eviction() {
        let store = ErrorStore::new(2);
        // Fill with errors
        store.add_entry(make_entry(ErrorLevel::Error, "err1"));
        store.add_entry(make_entry(ErrorLevel::Error, "err2"));
        assert_eq!(store.error_count(), 2);

        // Evict an error, add a warning
        store.add_entry(make_entry(ErrorLevel::Warning, "warn1"));
        assert_eq!(store.error_count(), 1);
        assert_eq!(store.warning_count(), 1);
    }

    #[test]
    fn test_thread_safety() {
        use std::thread;

        let store = ErrorStore::new(1000);
        let store_clone = ErrorStore {
            entries: store.entries.clone(),
            max_entries: store.max_entries,
        };

        let handle = thread::spawn(move || {
            for i in 0..100 {
                store_clone.add_entry(make_entry(
                    ErrorLevel::Error,
                    &format!("thread-error-{}", i),
                ));
            }
        });

        for i in 0..100 {
            store.add_entry(make_entry(
                ErrorLevel::Warning,
                &format!("main-warning-{}", i),
            ));
        }

        handle.join().unwrap();
        assert_eq!(store.get_all_entries().len(), 200);
        assert_eq!(store.error_count(), 100);
        assert_eq!(store.warning_count(), 100);
    }

    // --- format_timestamp tests ---

    #[test]
    fn test_format_timestamp_epoch() {
        let time = UNIX_EPOCH;
        assert_eq!(format_timestamp(time), "00:00:00");
    }

    #[test]
    fn test_format_timestamp_one_hour() {
        let time = UNIX_EPOCH + std::time::Duration::from_secs(3600);
        assert_eq!(format_timestamp(time), "01:00:00");
    }

    #[test]
    fn test_format_timestamp_wraps_at_24_hours() {
        // 25 hours should wrap to 01:00:00 (since hours = (secs/3600) % 24)
        let time = UNIX_EPOCH + std::time::Duration::from_secs(25 * 3600);
        assert_eq!(format_timestamp(time), "01:00:00");
    }

    #[test]
    fn test_format_timestamp_mixed() {
        // 13 hours, 45 minutes, 30 seconds
        let secs = 13 * 3600 + 45 * 60 + 30;
        let time = UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(format_timestamp(time), "13:45:30");
    }

    #[test]
    fn test_format_timestamp_zero_padded() {
        // 1 hour, 2 minutes, 3 seconds
        let secs = 1 * 3600 + 2 * 60 + 3;
        let time = UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(format_timestamp(time), "01:02:03");
    }

    #[test]
    fn test_format_timestamp_current_time_does_not_panic() {
        let result = format_timestamp(SystemTime::now());
        assert_eq!(result.len(), 8); // "HH:MM:SS"
        assert_eq!(&result[2..3], ":");
        assert_eq!(&result[5..6], ":");
    }

    #[test]
    fn test_format_timestamp_max_values() {
        // 23:59:59
        let secs = 23 * 3600 + 59 * 60 + 59;
        let time = UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(format_timestamp(time), "23:59:59");
    }
}
