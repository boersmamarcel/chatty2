use gpui::Global;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

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
