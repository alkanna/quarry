//! What Quarry keeps to undo agent changes: in memory only, since before images hold the data itself, and capped in
//! total bytes with the oldest entries going first. Separate from the user's own undo list.

use std::collections::VecDeque;

/// Entries by the Activity id of the call that made them, oldest first.
pub struct UndoStore<T> {
    entries: VecDeque<(u64, u64, T)>,
    total: u64,
}

impl<T> Default for UndoStore<T> {
    fn default() -> Self {
        UndoStore { entries: VecDeque::new(), total: 0 }
    }
}

impl<T> UndoStore<T> {
    /// Keeps `v` for call `id`, then drops the oldest entries until the total fits `cap`. Returns the ids dropped, which
    /// can be `id` itself when it alone is over the cap.
    pub fn push(&mut self, id: u64, bytes: u64, v: T, cap: u64) -> Vec<u64> {
        self.entries.push_back((id, bytes, v));
        self.total += bytes;
        self.trim(cap)
    }

    /// Drops the oldest entries until the total fits `cap`, after the cap was lowered in Settings.
    pub fn trim(&mut self, cap: u64) -> Vec<u64> {
        let mut gone = Vec::new();
        while self.total > cap
            && let Some((id, bytes, _)) = self.entries.pop_front()
        {
            self.total -= bytes;
            gone.push(id);
        }
        gone
    }

    /// Takes the entry out to run it; a failed undo puts it back with `push`.
    pub fn take(&mut self, id: u64) -> Option<T> {
        let i = self.entries.iter().position(|e| e.0 == id)?;
        let (_, bytes, v) = self.entries.remove(i)?;
        self.total -= bytes;
        Some(v)
    }

    #[cfg(test)]
    pub fn has(&self, id: u64) -> bool {
        self.entries.iter().any(|e| e.0 == id)
    }

    #[cfg(test)]
    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_total_cap_evicts_the_oldest_first() {
        let mut s = UndoStore::default();
        assert!(s.push(1, 40, "a", 100).is_empty());
        assert!(s.push(2, 40, "b", 100).is_empty());
        assert_eq!(s.push(3, 40, "c", 100), [1]);
        assert_eq!(s.total(), 80);
        assert!(!s.has(1) && s.has(2) && s.has(3));
        // One entry over the cap on its own goes at once, and takes everything older with it.
        assert_eq!(s.push(4, 150, "d", 100), [2, 3, 4]);
        assert_eq!(s.total(), 0);
    }

    #[test]
    fn take_frees_its_bytes_and_a_lower_cap_trims() {
        let mut s = UndoStore::default();
        s.push(1, 30, "a", 100);
        s.push(2, 30, "b", 100);
        s.push(3, 30, "c", 100);
        assert_eq!(s.take(2), Some("b"));
        assert_eq!(s.take(2), None);
        assert_eq!(s.total(), 60);
        // Put back after a failed undo, it counts as the newest.
        s.push(2, 30, "b", 100);
        assert_eq!(s.trim(40), [1, 3]);
        assert!(s.has(2));
    }
}
