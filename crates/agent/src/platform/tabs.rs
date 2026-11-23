//! Windows Terminal tells each tab's console host when the tab gains or loses focus, and the host reads that from the
//! terminal like any input. So a tab switch shows up between two polls as reads on two tabs, the window coming to the
//! front or going to the back as reads on the tab in use, and typing only reaches the tab in use.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tab {
    pub id: usize,
    pub window: usize,
    pub reads: Option<u64>,
}

#[derive(Default)]
pub struct ActiveTabs {
    reads: HashMap<usize, Option<u64>>,
    active: HashMap<usize, Vec<usize>>,
    polled: bool,
}

impl ActiveTabs {
    pub fn update(&mut self, tabs: &[Tab]) {
        let mut moved: HashMap<usize, Vec<usize>> = HashMap::new();
        for t in tabs {
            let read = match self.reads.get(&t.id) {
                Some(&before) => matches!((before, t.reads), (Some(b), Some(n)) if n != b),
                // A tab that opens while we watch is shown, and its first reads came before we saw it.
                None => self.polled,
            };
            if read {
                moved.entry(t.window).or_default().push(t.id);
            }
        }
        self.reads = tabs.iter().map(|t| (t.id, t.reads)).collect();
        let window_of: HashMap<usize, usize> = tabs.iter().map(|t| (t.id, t.window)).collect();
        self.active.retain(|window, ids| {
            ids.retain(|id| window_of.get(id) == Some(window));
            !ids.is_empty()
        });
        for (window, mut now) in moved {
            now.sort_unstable();
            let left: Vec<usize> =
                self.active.get(&window).into_iter().flatten().copied().filter(|id| now.contains(id)).collect();
            if now.len() > 1 && left.len() == 1 {
                now.retain(|id| *id != left[0]);
            }
            self.active.insert(window, now);
        }
        self.polled = true;
    }

    pub fn of(&self, window: usize) -> Option<&[usize]> {
        self.active.get(&window).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 100;

    fn poll(t: &mut ActiveTabs, tabs: &[(usize, usize, u64)]) {
        let tabs: Vec<Tab> = tabs.iter().map(|&(id, window, reads)| Tab { id, window, reads: Some(reads) }).collect();
        t.update(&tabs);
    }

    #[test]
    fn nothing_is_known_until_a_tab_reads() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9)]);
        assert_eq!(t.of(W), None, "tabs found at start tell nothing");
        poll(&mut t, &[(1, W, 5), (2, W, 9)]);
        assert_eq!(t.of(W), None);
    }

    #[test]
    fn the_window_coming_to_the_front_marks_its_tab() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9)]);
        poll(&mut t, &[(1, W, 5), (2, W, 10)]);
        assert_eq!(t.of(W), Some(&[2][..]));
        poll(&mut t, &[(1, W, 5), (2, W, 12)]);
        assert_eq!(t.of(W), Some(&[2][..]), "typing in it, or the window leaving and coming back");
        poll(&mut t, &[(1, W, 5), (2, W, 12)]);
        assert_eq!(t.of(W), Some(&[2][..]), "a quiet poll keeps it");
    }

    #[test]
    fn a_switch_moves_to_the_tab_that_was_not_in_use() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9), (3, W, 1)]);
        poll(&mut t, &[(1, W, 6), (2, W, 9), (3, W, 1)]);
        poll(&mut t, &[(1, W, 7), (2, W, 10), (3, W, 1)]);
        assert_eq!(t.of(W), Some(&[2][..]));
        poll(&mut t, &[(1, W, 7), (2, W, 11), (3, W, 2)]);
        assert_eq!(t.of(W), Some(&[3][..]));
    }

    #[test]
    fn switches_it_cannot_tell_apart_keep_every_tab_that_may_be_in_use() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9), (3, W, 1)]);
        poll(&mut t, &[(1, W, 6), (2, W, 10), (3, W, 1)]);
        assert_eq!(t.of(W), Some(&[1, 2][..]), "a switch before anything was known");
        poll(&mut t, &[(1, W, 6), (2, W, 11), (3, W, 2)]);
        assert_eq!(t.of(W), Some(&[3][..]), "only 2 of the two read again, so 2 was left for 3");
        poll(&mut t, &[(1, W, 7), (2, W, 12), (3, W, 3)]);
        assert_eq!(t.of(W), Some(&[1, 2][..]), "3 went to 1 then 2 in one poll");
        poll(&mut t, &[(1, W, 7), (2, W, 13), (3, W, 3)]);
        assert_eq!(t.of(W), Some(&[2][..]));
    }

    #[test]
    fn a_new_tab_is_the_one_shown() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5)]);
        poll(&mut t, &[(1, W, 6)]);
        poll(&mut t, &[(1, W, 7), (2, W, 3)]);
        assert_eq!(t.of(W), Some(&[2][..]));
    }

    #[test]
    fn closing_the_tab_in_use_moves_to_the_tab_shown_next() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9), (3, W, 1)]);
        poll(&mut t, &[(1, W, 5), (2, W, 10), (3, W, 1)]);
        poll(&mut t, &[(1, W, 5), (3, W, 1)]);
        assert_eq!(t.of(W), None, "the tab in use closed");
        poll(&mut t, &[(1, W, 5), (3, W, 2)]);
        assert_eq!(t.of(W), Some(&[3][..]));
    }

    #[test]
    fn windows_are_tracked_apart_and_unreadable_tabs_never_read() {
        let mut t = ActiveTabs::default();
        let tabs = |a: u64, b: u64, c: Option<u64>| {
            [Tab { id: 1, window: W, reads: Some(a) }, Tab { id: 2, window: 200, reads: Some(b) }, Tab { id: 3, window: W, reads: c }]
        };
        t.update(&tabs(5, 9, None));
        t.update(&tabs(6, 10, None));
        assert_eq!((t.of(W), t.of(200)), (Some(&[1][..]), Some(&[2][..])));
        t.update(&tabs(6, 10, Some(4)));
        assert_eq!(t.of(W), Some(&[1][..]), "a tab that becomes readable has no reads to compare yet");
    }

    #[test]
    fn a_tab_moved_to_another_window_leaves_the_first() {
        let mut t = ActiveTabs::default();
        poll(&mut t, &[(1, W, 5), (2, W, 9)]);
        poll(&mut t, &[(1, W, 5), (2, W, 10)]);
        poll(&mut t, &[(1, W, 5), (2, 200, 10)]);
        assert_eq!((t.of(W), t.of(200)), (None, None));
    }
}
