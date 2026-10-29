use std::collections::BTreeSet;

/// The selection after clicking row `ix` of the visible `ids`, or None if `ix` is out of range.
pub(super) fn next_selection(
    checked: &BTreeSet<u64>,
    anchor: Option<u64>,
    ids: &[u64],
    ix: usize,
    shift: bool,
    toggle: bool,
) -> Option<(BTreeSet<u64>, Option<u64>)> {
    let &id = ids.get(ix)?;
    let anchor_ix = anchor.and_then(|a| ids.iter().position(|&i| i == a));
    Some(match anchor_ix {
        Some(from) if shift => (ids[from.min(ix)..=from.max(ix)].iter().copied().collect(), anchor),
        _ if toggle => {
            let mut checked = checked.clone();
            if !checked.remove(&id) {
                checked.insert(id);
            }
            (checked, Some(id))
        }
        _ => (BTreeSet::from([id]), Some(id)),
    })
}

#[cfg(test)]
mod tests {
    use super::{BTreeSet, next_selection};

    #[test]
    fn click_toggle_and_range_selection() {
        let ids = [10, 20, 30, 40];
        let none = BTreeSet::new();
        let (sel, anchor) = next_selection(&none, None, &ids, 1, false, false).unwrap();
        assert_eq!((sel.iter().copied().collect::<Vec<_>>(), anchor), (vec![20], Some(20)));
        // ⌘-click adds, then removes.
        let (sel, anchor) = next_selection(&sel, anchor, &ids, 3, false, true).unwrap();
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), [20, 40]);
        let (sel, anchor) = next_selection(&sel, anchor, &ids, 3, false, true).unwrap();
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), [20]);
        // Shift-click from anchor 40 (set by the last toggle) back to 10 selects the whole span.
        let (sel, anchor) = next_selection(&sel, anchor, &ids, 0, true, false).unwrap();
        assert_eq!((sel.iter().copied().collect::<Vec<_>>(), anchor), (vec![10, 20, 30, 40], Some(40)));
        // Shift with no anchor acts like a plain click.
        let (sel, _) = next_selection(&sel, None, &ids, 2, true, false).unwrap();
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), [30]);
        assert!(next_selection(&sel, None, &ids, 9, false, false).is_none());
    }
}
