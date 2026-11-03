//! Open documents, and which one you are looking at.
//!
//! A viewer with tabs is a viewer with a *session*: an ordered list of open
//! documents, one of which is active. That is state, not UI, so it lives here
//! where it can be tested without a window.
//!
//! Two rules the model enforces so the UI never has to:
//!
//! * Opening a document that is already open focuses it instead of opening a
//!   second copy. Nobody wants two tabs of the same PDF.
//! * Pinned tabs sort to the left, keep their relative order, and refuse to be
//!   closed. Pinning is a promise, so closing one has to be deliberate
//!   (`close_forced`).

use std::path::PathBuf;

/// One open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// Stable identity, used to key cached tiles. Never reused, never reordered
    /// — which is why it exists instead of the tab's index.
    pub id: u32,
    pub path: PathBuf,
    pub title: String,
    pub pinned: bool,
}

#[derive(Debug, Default)]
pub struct Session {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
}

impl Session {
    pub fn new() -> Self {
        Session {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_index(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some(self.active)
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Open a document, or focus it if it is already open.
    ///
    /// Returns `(id, index, opened)` — `opened` is false when the document was
    /// already there, which is how the caller knows not to load it again.
    pub fn open(&mut self, path: PathBuf, title: String) -> (u32, usize, bool) {
        if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
            self.active = index;
            let tab = &self.tabs[index];
            return (tab.id, index, false);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            path,
            title,
            pinned: false,
        });
        self.active = self.tabs.len() - 1;
        (id, self.active, true)
    }

    /// Close a tab. Refuses pinned tabs, which is the whole point of pinning.
    pub fn close(&mut self, index: usize) -> bool {
        match self.tabs.get(index) {
            Some(tab) if tab.pinned => false,
            Some(_) => self.close_forced(index),
            None => false,
        }
    }

    pub fn close_forced(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active > index {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    /// Close the active tab. Returns the id that was closed, if any.
    pub fn close_active(&mut self) -> Option<u32> {
        let index = self.active_index()?;
        let id = self.tabs[index].id;
        if self.close(index) {
            Some(id)
        } else {
            None
        }
    }

    /// Pin or unpin a tab, then re-sort so pinned tabs sit on the left.
    pub fn set_pinned(&mut self, index: usize, pinned: bool) {
        if index >= self.tabs.len() {
            return;
        }
        let id = self.tabs[index].id;
        self.tabs[index].pinned = pinned;
        self.sort_pinned();
        self.focus_id(id);
    }

    pub fn toggle_pinned_active(&mut self) {
        if let Some(index) = self.active_index() {
            let pinned = self.tabs[index].pinned;
            self.set_pinned(index, !pinned);
        }
    }

    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// Move focus to the tab with this id, wherever it has sorted to.
    pub fn focus_id(&mut self, id: u32) {
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.active = index;
        }
    }

    pub fn next(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
    }

    pub fn prev(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = if self.active == 0 {
            self.tabs.len() - 1
        } else {
            self.active - 1
        };
    }

    /// Pinned tabs first, each group in the order it was already in.
    fn sort_pinned(&mut self) {
        self.tabs.sort_by_key(|t| !t.pinned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(paths: &[&str]) -> Session {
        let mut s = Session::new();
        for p in paths {
            s.open(PathBuf::from(p), p.to_string());
        }
        s
    }

    #[test]
    fn opening_a_duplicate_focuses_instead_of_duplicating() {
        let mut s = session_with(&["a.pdf", "b.pdf"]);
        let (id, index, opened) = s.open(PathBuf::from("a.pdf"), "a.pdf".into());
        assert!(!opened);
        assert_eq!(index, 0);
        assert_eq!(s.len(), 2);
        assert_eq!(s.active_index(), Some(0));
        assert_eq!(s.active_tab().map(|t| t.id), Some(id));
    }

    #[test]
    fn ids_are_stable_across_reordering() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        let b_id = s.tabs()[1].id;
        s.set_pinned(1, true);
        assert_eq!(s.tabs()[0].id, b_id, "pinned tab should sort to the front");
        assert_eq!(s.active_tab().map(|t| t.id), Some(b_id), "focus follows the tab");
    }

    #[test]
    fn pinned_tabs_refuse_to_close() {
        let mut s = session_with(&["a.pdf", "b.pdf"]);
        s.set_pinned(0, true);
        assert!(!s.close(0));
        assert_eq!(s.len(), 2);
        assert!(s.close_forced(0));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn closing_the_active_tab_lands_on_a_neighbour() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        s.activate(2);
        assert!(s.close(2));
        assert_eq!(s.active_index(), Some(1));
        assert_eq!(s.active_tab().map(|t| t.title.as_str()), Some("b.pdf"));
    }

    #[test]
    fn closing_before_the_active_tab_keeps_the_same_document_active() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        s.activate(2);
        let active_id = s.active_tab().unwrap().id;
        assert!(s.close(0));
        assert_eq!(s.active_tab().map(|t| t.id), Some(active_id));
    }

    #[test]
    fn closing_everything_leaves_no_active_tab() {
        let mut s = session_with(&["a.pdf"]);
        assert!(s.close(0));
        assert!(s.is_empty());
        assert_eq!(s.active_index(), None);
        assert_eq!(s.active_tab(), None);
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        s.activate(0);
        s.prev();
        assert_eq!(s.active_index(), Some(2));
        s.next();
        assert_eq!(s.active_index(), Some(0));
    }

    fn titles(s: &Session) -> Vec<&str> {
        s.tabs().iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn pinning_sorts_into_the_left_group_keeping_order_within_it() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        s.set_pinned(1, true);
        s.set_pinned(2, true);
        assert_eq!(titles(&s), vec!["b.pdf", "c.pdf", "a.pdf"]);
    }

    #[test]
    fn unpinning_moves_a_tab_to_the_front_of_the_unpinned_group() {
        let mut s = session_with(&["a.pdf", "b.pdf", "c.pdf"]);
        s.set_pinned(1, true);
        s.set_pinned(2, true);
        assert_eq!(titles(&s), vec!["b.pdf", "c.pdf", "a.pdf"]);

        let b_id = s.tabs()[0].id;
        s.set_pinned(0, false);
        // c is still pinned, so b lands first among the unpinned ones.
        assert_eq!(titles(&s), vec!["c.pdf", "b.pdf", "a.pdf"]);
        assert_eq!(s.tabs()[1].id, b_id);
        assert_eq!(s.active_tab().map(|t| t.id), Some(b_id));
    }
}
