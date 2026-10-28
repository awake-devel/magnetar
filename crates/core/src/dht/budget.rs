//! How the KRPC query budget is split between BEP 51 sampling and everything else (lookups,
//! pings, refreshes): the one place that decides it (D62, D106).
//!
//! The engine's global [`super::limiter::Pacer`] caps the total. Two knobs, both here:
//!
//! - `share` (D62, set by the server through `Dht::set_sample_share`: 0 while the fetch queue is
//!   backlogged, 1 again at half of that): the sampler may use at most this share of the cap,
//!   at evenly spaced instants, and waits in the pacer's queue like every other query;
//! - `priority` (D106, `Dht::set_sample_priority`, `MAGNETAR_DHT_SAMPLE_PRIORITY`, default 0):
//!   a share of the cap reserved for the sampler, sent **ahead of the queue** and charged to it
//!   (`Pacer::take_ahead`). Without it, thousands of queued lookup queries leave the sampler one
//!   send per queue wait: about 1 % of the queries on the live line (docs/DHT.md §20).
//!
//! Lookups take whatever the pacer gives them. A smarter policy (a balancer) only needs to call
//! the two setters, or to replace [`SampleBudget::next_slot`].

use std::time::Duration;

use tokio::time::Instant;

/// The sampler's share of the query budget and its own schedule.
#[derive(Debug)]
pub struct SampleBudget {
    /// Share of the QPS budget the sampler may use, 0–1 (0 pauses sampling).
    share: f64,
    /// Share of the QPS budget reserved for the sampler ahead of the queue, 0–1 (0 = none).
    priority: f64,
    next: Instant,
}

impl SampleBudget {
    pub fn new(now: Instant) -> Self {
        Self {
            share: 1.0,
            priority: 0.0,
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

    pub fn priority(&self) -> f64 {
        self.priority
    }

    /// Set the reserved share (clamped to 0–1, NaN = 0). A change restarts the schedule now.
    pub fn set_priority(&mut self, priority: f64, now: Instant) {
        let p = if priority.is_nan() {
            0.0
        } else {
            priority.clamp(0.0, 1.0)
        };
        if p != self.priority {
            self.priority = p;
            self.next = now;
        }
    }

    /// When the sampler may send its next query, given the total cap `qps` (halved while
    /// braked), and whether it goes ahead of the pacer's queue; `None` while paused.
    /// With priority, sends are spaced at `qps × share × priority`; without, at share 1 the
    /// sampler is only bounded by the global pacer.
    pub fn next_slot(&mut self, qps: f64, now: Instant) -> Option<(Instant, bool)> {
        if self.share <= 0.0 {
            return None;
        }
        let at = self.next.max(now);
        let ahead = self.priority > 0.0;
        let rate = if ahead {
            qps.max(1.0) * self.share * self.priority
        } else {
            qps.max(1.0) * self.share
        };
        if ahead || self.share < 1.0 {
            self.next = at + Duration::from_secs_f64(1.0 / rate);
        }
        Some((at, ahead))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_spaces_the_sampler_and_zero_pauses() {
        let t = Instant::now();
        let mut b = SampleBudget::new(t);
        assert_eq!(b.next_slot(100.0, t), Some((t, false)));
        assert_eq!(b.next_slot(100.0, t), Some((t, false))); // share 1: only the pacer bounds it
        b.set_share(0.5, t);
        assert_eq!(b.next_slot(100.0, t), Some((t, false)));
        assert_eq!(
            b.next_slot(100.0, t),
            Some((t + Duration::from_millis(20), false))
        );
        b.set_share(0.0, t);
        assert_eq!(b.next_slot(100.0, t), None);
        b.set_share(f64::NAN, t);
        assert_eq!(b.share(), 1.0);
    }

    #[test]
    fn priority_reserves_a_spaced_share_ahead_of_the_queue() {
        let t = Instant::now();
        let mut b = SampleBudget::new(t);
        b.set_priority(0.25, t);
        assert_eq!(b.next_slot(1000.0, t), Some((t, true)));
        assert_eq!(
            b.next_slot(1000.0, t),
            Some((t + Duration::from_millis(4), true))
        );
        // Backpressure still pauses it, and halves it at share 0.5.
        b.set_share(0.0, t);
        assert_eq!(b.next_slot(1000.0, t), None);
        b.set_share(0.5, t);
        assert_eq!(b.next_slot(1000.0, t), Some((t, true)));
        assert_eq!(
            b.next_slot(1000.0, t),
            Some((t + Duration::from_millis(8), true))
        );
        b.set_priority(f64::NAN, t);
        assert_eq!(b.priority(), 0.0);
    }
}
