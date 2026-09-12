//! Frecency: how often an app is launched, discounted by how long ago.

use std::collections::HashMap;

const SECS_PER_DAY: f64 = 86_400.0;

/// How much a maximally-used app may multiply its fuzzy match score.
///
/// Multiplicative rather than additive, and that is the load-bearing choice. Measured
/// score spreads over the real index: `"s"` returns ten results that all tie at exactly
/// 36, `"sa"` spans 62 down to 38, `"term"` spans 114 down to 64. An additive bonus big
/// enough to break the zero-spread tie at `"s"` is necessarily also big enough to drag a
/// barely-matching favourite over a far better textual match — typing `"x"` would put
/// Firefox above Xcode. Scaling keeps text dominant instead: a 16-point match at full
/// frecency reaches 25.6 and still loses to a 36-point match with no history at all.
const WEIGHT: f64 = 0.6;

/// The decayed launch count at which the boost reaches half its maximum.
///
/// Saturating, so the first few launches move the needle and the fiftieth does not.
/// Never-launched to launched-once is a real distinction; 40 launches to 41 is not.
const SATURATION: f64 = 3.0;

/// One app's launch history, collapsed to a single number.
///
/// A score plus the moment it was current, rather than a list of timestamps. Decaying
/// the stored value to "now" and adding one is arithmetically identical to summing
/// `0.5^(age / half_life)` over every launch that ever happened, but it is O(1) in both
/// storage and time and never needs pruning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visit {
    /// The decayed launch count as of `updated`.
    pub score: f64,
    /// Unix seconds at which `score` was last brought up to date.
    pub updated: u64,
}

/// Launch history for every app, in memory.
#[derive(Debug, Clone)]
pub struct Frecency {
    half_life_secs: f64,
    visits: HashMap<u64, Visit>,
}

impl Frecency {
    pub fn new(half_life_days: f64) -> Self {
        Self {
            half_life_secs: half_life_days * SECS_PER_DAY,
            visits: HashMap::new(),
        }
    }

    /// Changes the decay curve without touching the history.
    ///
    /// Visits are stored raw and decayed at read time, so a new half-life is a one-field
    /// recompute rather than a rebuild — which is what lets the settings window make this
    /// one live.
    pub fn set_half_life(&mut self, half_life_days: f64) {
        self.half_life_secs = half_life_days * SECS_PER_DAY;
    }

    /// Rehydrates from whatever the store held.
    pub fn load(half_life_days: f64, visits: impl IntoIterator<Item = (u64, Visit)>) -> Self {
        Self {
            visits: visits.into_iter().collect(),
            ..Self::new(half_life_days)
        }
    }

    /// Records a launch, returning the row the caller should persist.
    pub fn record(&mut self, id: u64, now: u64) -> Visit {
        let visit = Visit {
            score: self.score(id, now) + 1.0,
            updated: now,
        };
        self.visits.insert(id, visit);
        visit
    }

    /// The decayed launch count as of `now`. Zero for an app never launched.
    pub fn score(&self, id: u64, now: u64) -> f64 {
        self.visits.get(&id).map_or(0.0, |v| {
            decay(v.score, elapsed(v.updated, now), self.half_life_secs)
        })
    }

    /// The multiplier to apply to a fuzzy match score, in `[1.0, 1.0 + WEIGHT)`.
    pub fn boost(&self, id: u64, now: u64) -> f64 {
        boost_for(self.score(id, now))
    }

    /// As [`Frecency::boost`], taking the larger of this history and `other` — Spotlight's
    /// record of the same app. The larger, not the sum: a launch through blindspot is also
    /// a launch macOS records, so adding them would count it twice.
    pub fn boost_with(&self, id: u64, now: u64, other: f64) -> f64 {
        boost_for(self.score(id, now).max(other))
    }

    pub fn len(&self) -> usize {
        self.visits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visits.is_empty()
    }
}

/// A history of use dates collapsed to one score with the same half-life decay a recorded
/// launch gets — so Spotlight's history and blindspot's own are measured in the same units.
pub(crate) fn decayed_count(dates: &[u64], now: u64, half_life_days: f64) -> f64 {
    let half_life_secs = half_life_days * SECS_PER_DAY;
    dates
        .iter()
        .map(|&date| decay(1.0, elapsed(date, now), half_life_secs))
        .sum()
}

/// Saturating, so a clock that jumped backwards reads as "no time passed" rather than a
/// negative age, which would inflate the score instead of decaying it.
fn elapsed(from: u64, to: u64) -> f64 {
    to.saturating_sub(from) as f64
}

fn decay(score: f64, elapsed_secs: f64, half_life_secs: f64) -> f64 {
    // A non-positive half-life means "never decay". A typo in config.toml should cost
    // the user their decay curve, not their ranking — and instant decay to zero would
    // silently disable frecency altogether, which is far harder to notice.
    if half_life_secs <= 0.0 {
        return score;
    }
    score * 0.5_f64.powf(elapsed_secs / half_life_secs)
}

fn boost_for(frecency: f64) -> f64 {
    if frecency <= 0.0 {
        return 1.0;
    }
    1.0 + WEIGHT * (frecency / (frecency + SATURATION))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn a_launch_adds_one_and_an_unlaunched_app_scores_zero() {
        let mut f = Frecency::new(14.0);
        assert_eq!(f.score(1, 0), 0.0);
        f.record(1, 0);
        assert!((f.score(1, 0) - 1.0).abs() < 1e-9);
        f.record(1, 0);
        assert!((f.score(1, 0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn score_halves_over_one_half_life() {
        let mut f = Frecency::new(14.0);
        f.record(1, 0);
        assert!((f.score(1, 14 * DAY) - 0.5).abs() < 1e-9);
        assert!((f.score(1, 28 * DAY) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn recording_decays_what_was_there_before_adding_to_it() {
        let mut f = Frecency::new(14.0);
        f.record(1, 0);
        // One half-life later the old launch is worth 0.5, plus this one.
        let visit = f.record(1, 14 * DAY);
        assert!((visit.score - 1.5).abs() < 1e-9);
        assert_eq!(visit.updated, 14 * DAY);
    }

    #[test]
    fn a_backwards_clock_neither_panics_nor_inflates() {
        let mut f = Frecency::new(14.0);
        f.record(1, 100 * DAY);
        assert!(
            (f.score(1, 0) - 1.0).abs() < 1e-9,
            "must not exceed what was recorded"
        );
    }

    #[test]
    fn a_nonsensical_half_life_disables_decay_rather_than_ranking() {
        for half_life in [0.0, -5.0] {
            let mut f = Frecency::new(half_life);
            f.record(1, 0);
            assert!((f.score(1, 9999 * DAY) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn the_boost_is_bounded_and_monotonic() {
        assert_eq!(boost_for(0.0), 1.0);
        assert_eq!(boost_for(-1.0), 1.0);
        let mut previous = 1.0;
        for launches in [0.5, 1.0, 3.0, 10.0, 100.0, 10_000.0] {
            let b = boost_for(launches);
            assert!(b > previous, "boost must rise with use");
            assert!(b < 1.0 + WEIGHT, "boost must stay bounded");
            previous = b;
        }
        // Half the maximum at exactly the saturation point.
        assert!((boost_for(SATURATION) - (1.0 + WEIGHT / 2.0)).abs() < 1e-9);
    }

    #[test]
    fn frecency_cannot_rescue_a_much_worse_textual_match() {
        // The real numbers behind WEIGHT: typing "x" scores Xcode 36 and Firefox 16.
        let best_possible = 16.0 * boost_for(f64::MAX);
        assert!(
            best_possible < 36.0,
            "a favourite 16-point match ({best_possible}) must still lose to a cold 36"
        );
    }

    #[test]
    fn frecency_decides_a_tie() {
        // Typing "s" returns ten results all scoring exactly 36.
        let mut f = Frecency::new(14.0);
        f.record(7, 0);
        assert!(36.0 * f.boost(7, 0) > 36.0 * f.boost(9, 0));
    }

    #[test]
    fn a_reloaded_store_scores_the_same() {
        let mut f = Frecency::new(14.0);
        f.record(1, 0);
        f.record(1, DAY);
        let rows: Vec<(u64, Visit)> = f.visits.iter().map(|(k, v)| (*k, *v)).collect();
        let reloaded = Frecency::load(14.0, rows);
        assert_eq!(reloaded.score(1, 5 * DAY), f.score(1, 5 * DAY));
        assert_eq!(reloaded.len(), 1);
    }
}
