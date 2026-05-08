//! Per-account drift observation window.
//!
//! Implements the consecutive-observation rule from design § 3 (hard guards).
//! A drift exceeding the per-asset dust threshold must be observed in
//! `threshold` consecutive ticks (default 3) with the same sign and within
//! 50% of magnitude before triggering an adjustment. Sign flips and large
//! magnitude swings reset the window.

use std::collections::VecDeque;

/// Outcome of one drift observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftOutcome {
    /// Drift was within the dust threshold; window is reset.
    WithinDust,
    /// Drift was above dust; window has not yet reached the trigger threshold.
    Building {
        /// How many consecutive observations are currently held.
        observations: u8,
    },
    /// Window reached the trigger threshold — caller should attempt to post
    /// an adjustment with this drift value (the per-window arithmetic mean).
    Trigger {
        /// Signed drift to apply (averaged across the window).
        drift: i128,
    },
}

/// Sliding observation window for drift tracking.
///
/// The window keeps the last `threshold` observations. On each `observe`:
/// - within-dust drifts clear the window and return `WithinDust`;
/// - sign flips or magnitude swings >50% relative to the previous
///   observation clear the window before recording the new value;
/// - reaching `threshold` same-sign, in-magnitude observations returns
///   `Trigger { drift }` and clears the window for the next cycle.
#[derive(Debug, Clone)]
pub struct DriftWindow {
    dust: u128,
    threshold: u8,
    history: VecDeque<i128>,
}

impl DriftWindow {
    /// Create a window with the default 3-observation threshold.
    #[must_use]
    pub fn new(dust: u128) -> Self {
        Self::with_threshold(dust, 3)
    }

    /// Create a window with an explicit threshold.
    ///
    /// The minimum effective threshold is 1 — a 0 value is clamped up to
    /// keep the trigger semantics well-defined.
    #[must_use]
    pub fn with_threshold(dust: u128, threshold: u8) -> Self {
        let threshold = threshold.max(1);
        Self {
            dust,
            threshold,
            history: VecDeque::with_capacity(threshold as usize),
        }
    }

    /// Reset the observation history.
    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// Record one drift observation and return the resulting outcome.
    pub fn observe(&mut self, drift: i128) -> DriftOutcome {
        if drift.unsigned_abs() <= self.dust {
            self.history.clear();
            return DriftOutcome::WithinDust;
        }

        if let Some(&last) = self.history.back() {
            let same_sign = (last >= 0) == (drift >= 0);
            let last_abs = last.unsigned_abs();
            let drift_abs = drift.unsigned_abs();
            let max = last_abs.max(drift_abs);
            let min = last_abs.min(drift_abs);
            // Within 50% of magnitude: |max - min| / max <= 0.5
            // Equivalent: 2 * (max - min) <= max
            let within_50_pct = max == 0 || (max - min).saturating_mul(2) <= max;
            if !same_sign || !within_50_pct {
                self.history.clear();
            }
        }

        if self.history.len() == self.threshold as usize {
            self.history.pop_front();
        }
        self.history.push_back(drift);

        if self.history.len() == self.threshold as usize {
            // Average the window for a stable adjustment magnitude. Saturating
            // to i128 max/min defends against pathological inputs.
            let len = i128::try_from(self.history.len()).unwrap_or(1);
            let sum: i128 = self.history.iter().sum();
            let avg = sum / len;
            self.history.clear();
            DriftOutcome::Trigger { drift: avg }
        } else {
            let count = u8::try_from(self.history.len()).unwrap_or(u8::MAX);
            DriftOutcome::Building {
                observations: count,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_triggers_after_three_same_sign_observations_above_dust() {
        let mut window = DriftWindow::new(100);
        assert_eq!(window.observe(50), DriftOutcome::WithinDust);
        assert_eq!(
            window.observe(200),
            DriftOutcome::Building { observations: 1 }
        );
        assert_eq!(
            window.observe(210),
            DriftOutcome::Building { observations: 2 }
        );
        assert_eq!(window.observe(205), DriftOutcome::Trigger { drift: 205 });
    }

    #[test]
    fn window_resets_on_sign_flip() {
        let mut window = DriftWindow::new(100);
        window.observe(200);
        window.observe(210);
        assert_eq!(
            window.observe(-200),
            DriftOutcome::Building { observations: 1 },
            "sign flip must reset the window before recording the new sample"
        );
    }

    #[test]
    fn window_resets_on_magnitude_swing_over_50_percent() {
        let mut window = DriftWindow::new(100);
        window.observe(200);
        window.observe(210);
        // 500 vs 210 — relative swing is (500-210)/500 = 0.58 > 0.5.
        assert_eq!(
            window.observe(500),
            DriftOutcome::Building { observations: 1 },
            "out-of-magnitude swing must reset the window"
        );
    }

    #[test]
    fn window_below_dust_is_within_dust() {
        let mut window = DriftWindow::new(100);
        assert_eq!(window.observe(50), DriftOutcome::WithinDust);
        assert_eq!(window.observe(-50), DriftOutcome::WithinDust);
        // Boundary: dust itself is not above the threshold.
        assert_eq!(window.observe(100), DriftOutcome::WithinDust);
    }
}
