use crate::money::Cents;

/// Split `target` across contributions so that every share is `min(c, level)`
/// for one common level, with leftover cents given one each to the earliest
/// contributors above the level.
///
/// If contributions do not exceed `target`, nobody is charged retroactively:
/// shares equal contributions and the period is short.
pub fn water_fill(contributions: &[Cents], target: Cents) -> Vec<Cents> {
    let total: Cents = contributions.iter().sum();
    if total <= target {
        return contributions.to_vec();
    }
    let used_at = |level: Cents| -> Cents { contributions.iter().map(|&c| c.min(level)).sum() };

    // Largest level whose total does not exceed the target; used_at is monotone.
    let (mut lo, mut hi) = (0, contributions.iter().copied().max().unwrap_or(0));
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if used_at(mid) <= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let level = lo;
    let mut remainder = target - used_at(level);
    contributions
        .iter()
        .map(|&c| {
            let mut share = c.min(level);
            // used_at(level + 1) > target, so strictly more than `remainder`
            // contributors sit above the level and the loop always drains it.
            if c > level && remainder > 0 {
                share += 1;
                remainder -= 1;
            }
            share
        })
        .collect()
}
