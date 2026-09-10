//! Select cheap borrowed candidates before constructing bridge payloads.

pub(super) fn select<T: Copy>(
    units: impl IntoIterator<Item = (f32, bool, T)>,
    radius2: f32,
    limit: usize,
) -> (Vec<(f32, T)>, Option<(f32, T)>) {
    let mut candidates = Vec::new();
    let mut target = None;
    for (order, (distance2, is_target, unit)) in units.into_iter().enumerate() {
        if is_target {
            target = Some((distance2, unit));
        }
        if limit > 0 && distance2 <= radius2 {
            candidates.push((distance2, order, unit));
        }
    }
    // Enumeration order is a secondary key: this matches the old stable distance sort
    // even at the cutoff, while selection avoids sorting every nearby unit.
    let compare =
        |a: &(f32, usize, T), b: &(f32, usize, T)| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1));
    if candidates.len() > limit {
        candidates.select_nth_unstable_by(limit, compare);
        candidates.truncate(limit);
    }
    candidates.sort_unstable_by(compare);
    (
        candidates
            .into_iter()
            .map(|(d, _, unit)| (d, unit))
            .collect(),
        target,
    )
}

#[cfg(test)]
mod tests {
    use super::select;

    #[test]
    fn nearest_matches_stable_reference_for_every_cutoff() {
        let units: Vec<_> = (0..1000)
            .map(|i| ((i * 37 % 131) as f32, i == 997, i))
            .collect();
        let mut reference: Vec<_> = units
            .iter()
            .filter(|(d, _, _)| *d <= 90.0)
            .map(|&(d, _, unit)| (d, unit))
            .collect();
        reference.sort_by(|a, b| a.0.total_cmp(&b.0));
        for limit in [0, 1, 64, 500, 1000] {
            let (selected, target) = select(units.iter().copied(), 90.0, limit);
            assert_eq!(selected, reference[..reference.len().min(limit)]);
            assert_eq!(target, Some((units[997].0, 997)));
        }
    }

    #[test]
    fn target_survives_radius_and_zero_cap() {
        for limit in [0, 64] {
            let (selected, target) = select([(1.0, false, 1), (100.0, true, 2)], 4.0, limit);
            assert_eq!(selected.len(), usize::from(limit > 0));
            assert_eq!(target, Some((100.0, 2)));
        }
    }

    #[test]
    fn nonfinite_distances_do_not_enter_a_finite_radius_but_target_is_kept() {
        let (selected, target) = select([(f32::NAN, true, 1), (f32::INFINITY, false, 2)], 4.0, 64);
        assert!(selected.is_empty());
        assert!(target.unwrap().0.is_nan());
        assert_eq!(select::<usize>([], 4.0, 64), (vec![], None));
    }
}
