//! How the KRPC query budget is split between BEP 51 sampling and everything else (lookups,
//! pings, refreshes): the one place that decides it (D62, D106).
//!
//! The engine's global [`super::limiter::Pacer`] caps the total; on top of that the sampler
//! may only use `share` of the cap, at evenly spaced instants. Lookups take whatever the pacer
//! gives them. The server sets the share (`Dht::set_sample_share`: 0 while the fetch queue is
//! backlogged, 1 again at half of that, D62); a smarter policy only needs to call that, or to
//! replace [`SampleBudget::next_slot`].

use std::time::Duration;

use tokio::time::Instant;

/// The sampler's share of the query budget and its own schedule.
#[derive(Debug)]
pub struct SampleBudget {
    /// Share of the QPS budget the sampler may use, 0–1 (0 pauses sampling).
    share: f64,
    next: Instant,
}

impl SampleBudget {
    pub fn new(now: Instant) -> Self {
        Self {
            share: 1.0,
            next: now,
        }
    }

    pub fn share(&self) -> f64 {
        self.share
    }

    /// Set the share (clamped to 0–1, NaN = 1). A change restarts the schedule now.
    pub fn set_share(&mut self, share: f64, now: Instant) {
        let share = if share.is_nan() {
            1.0
        } else {
            share.clamp(0.0, 1.0)
        };
        if share != self.share {
            self.share = share;
            self.next = now;
        }
    }

    /// When the sampler may send its next query, given the total cap `qps`; `None` while paused.
    /// At share 1 the sampler is only bounded by the global pacer.
    pub fn next_slot(&mut self, qps: f64, now: Instant) -> Option<Instant> {
        if self.share <= 0.0 {
            return None;
        }
        let at = self.next.max(now);
        if self.share < 1.0 {
            self.next = at + Duration::from_secs_f64(1.0 / (qps.max(1.0) * self.share));
        }
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_spaces_the_sampler_and_zero_pauses() {
        let t = Instant::now();
        let mut b = SampleBudget::new(t);
        assert_eq!(b.next_slot(100.0, t), Some(t));
        assert_eq!(b.next_slot(100.0, t), Some(t)); // share 1: only the pacer bounds it
        b.set_share(0.5, t);
        assert_eq!(b.next_slot(100.0, t), Some(t));
        assert_eq!(b.next_slot(100.0, t), Some(t + Duration::from_millis(20)));
        b.set_share(0.0, t);
        assert_eq!(b.next_slot(100.0, t), None);
        b.set_share(f64::NAN, t);
        assert_eq!(b.share(), 1.0);
    }
}
