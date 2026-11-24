//! One allocation per distinct process name, reused across snapshots.
//!
//! The ring keeps every process for 60 seconds. Copying each name into every
//! slot dominates that cost once the list is long. The table hands out an
//! `Arc` and drops names that the newest sample did not use. Older snapshots
//! keep their own `Arc` until the ring overwrites them, so an exited process
//! still has its name, and a reused PID with a new name does not alias the old one.

use std::collections::HashMap;

use crate::model::SharedName;

#[derive(Debug, Default)]
pub struct NameTable {
    current: HashMap<String, SharedName>,
    next: HashMap<String, SharedName>,
}

impl NameTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a sample. Names used by [`Self::share`] land in a fresh map.
    pub fn begin(&mut self) {
        self.next.clear();
    }

    pub fn share(&mut self, name: &str) -> SharedName {
        if let Some(found) = self.next.get(name) {
            return found.clone();
        }
        let shared = if let Some(found) = self.current.get(name) {
            found.clone()
        } else {
            SharedName::from(name)
        };
        self.next.insert(name.to_string(), shared.clone());
        shared
    }

    /// Keep only names from the sample that just finished.
    pub fn end(&mut self) {
        std::mem::swap(&mut self.current, &mut self.next);
        self.next.clear();
    }

    pub fn len(&self) -> usize {
        self.current.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_same_text_keeps_one_allocation_across_samples() {
        let mut table = NameTable::new();
        table.begin();
        let first = table.share("app.exe");
        let again = table.share("app.exe");
        table.end();
        table.begin();
        let later = table.share("app.exe");
        table.end();
        assert!(Arc::ptr_eq(first.as_arc(), again.as_arc()));
        assert!(Arc::ptr_eq(first.as_arc(), later.as_arc()));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_name_that_left_is_dropped_from_the_table() {
        let mut table = NameTable::new();
        table.begin();
        let gone = table.share("gone.exe");
        table.end();
        table.begin();
        let _live = table.share("live.exe");
        table.end();
        assert_eq!(table.len(), 1);
        assert_eq!(gone.as_str(), "gone.exe");
    }
}
