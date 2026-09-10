//! Wenilla carry: stable listener walks are linear; mutation retains saved-handle semantics.
pub(super) fn position<T: PartialEq>(list: &[T], saved: &T, hint: usize) -> Option<usize> {
    if list.get(hint) == Some(saved) {
        Some(hint)
    } else {
        list.iter().position(|item| item == saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_preserve_saved_successor() {
        assert_eq!(position(&[1, 2, 3], &2, 1), Some(1));
        // Current callback removed itself; its saved successor shifted left.
        assert_eq!(position(&[2, 3], &2, 1), Some(0));
        // Removing the saved successor must terminate the walk.
        assert_eq!(position(&[1, 3], &2, 1), None);
        assert_eq!(position(&[1, 2, 3, 4], &3, 2), Some(2));
        assert_eq!(position(&[], &2, 1), None);
    }

    #[test]
    fn unchanged_walk_uses_one_comparison_per_listener() {
        use std::cell::Cell;
        struct Counted<'a>(usize, &'a Cell<usize>);
        impl PartialEq for Counted<'_> {
            fn eq(&self, other: &Self) -> bool {
                self.1.set(self.1.get() + 1);
                self.0 == other.0
            }
        }
        let comparisons = Cell::new(0);
        let rows: Vec<_> = (0..1000).map(|i| Counted(i, &comparisons)).collect();
        for (i, item) in rows.iter().enumerate() {
            assert_eq!(position(&rows, item, i), Some(i));
        }
        assert_eq!(comparisons.get(), rows.len());
    }
}
