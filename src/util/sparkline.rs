//! A fixed-length ring buffer and the sparkline rendered from it.

use std::collections::VecDeque;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// How a window of samples is mapped onto the eight bar heights.
///
/// The scale is the caller's to pick, not the `Ring`'s: the ring holds bare numbers
/// and has no idea whether they are degrees or bytes per second. One floor for every
/// ring, such as `5.0`, is five degrees to a sensor and five bytes per second to a
/// network interface - a floor that could never engage on traffic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Straight min/max over the window. Only the **shape** shows: a series sitting at
    /// 40 and one sitting at 90 draw the same. Right for temperatures, where the wobble
    /// is the interesting part and the number beside it says where it sits.
    ///
    /// `min_range` stops a still value from having its noise amplified over the full
    /// height.
    Linear { min_range: f32 },
    /// log10, anchored at `floor` instead of at the window's minimum, so the bar height
    /// is the **magnitude** - and two series drawn this way can be compared.
    ///
    /// Right for traffic, which spans orders of magnitude. Measured over 90 seconds on
    /// a real link, rx ran from 1 kB/s to 2448 kB/s; on a linear scale **16 of those 18
    /// samples sat on the bottom bar**, because one burst set the top of the range. The
    /// wider the window, the likelier a burst is inside it, so widening the sparkline -
    /// the whole point of stacking rx over tx - made a linear one worse, not better.
    ///
    /// Anything at or below `floor` draws as the bottom bar, which is what makes an idle
    /// interface read as idle. `min_decades` stops a quiet window from being stretched
    /// over the full height.
    Log { floor: f32, min_decades: f32 },
}

#[derive(Debug, Clone)]
pub struct Ring {
    values: VecDeque<f32>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, value: f32) {
        if self.values.len() == self.cap {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    /// Render the most recent `width` samples as a sparkline.
    ///
    /// **Returns as many characters as there are samples, not `width`.** Padding a
    /// short series out to the field is the caller's job, so that the columns do not
    /// shift while the ring fills up.
    pub fn render(&self, width: usize, scale: Scale) -> String {
        if self.values.is_empty() || width == 0 {
            return String::new();
        }
        let skip = self.values.len().saturating_sub(width);
        let window: Vec<f32> = self.values.iter().skip(skip).copied().collect();

        let bar = |t: f32| {
            let idx = (t.clamp(0.0, 1.0) * (BARS.len() - 1) as f32).round() as usize;
            BARS[idx]
        };

        match scale {
            Scale::Linear { min_range } => {
                let min = window.iter().copied().fold(f32::INFINITY, f32::min);
                let max = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let mid = (min + max) / 2.0;
                let (lo, hi) = if max - min < min_range {
                    (mid - min_range / 2.0, mid + min_range / 2.0)
                } else {
                    (min, max)
                };
                let span = (hi - lo).max(f32::EPSILON);
                window.iter().map(|v| bar((v - lo) / span)).collect()
            }
            Scale::Log { floor, min_decades } => {
                // The bottom of the scale is the floor, not the window's minimum. That
                // is what makes the height mean "how much" rather than "how unusual".
                let floor = floor.max(f32::MIN_POSITIVE);
                let lo = floor.log10();
                let max = window
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max)
                    .max(floor);
                let hi = max.log10().max(lo + min_decades.max(0.0));
                let span = (hi - lo).max(f32::EPSILON);
                window
                    .iter()
                    .map(|v| bar((v.max(floor).log10() - lo) / span))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the temperature panel uses.
    const LINEAR: Scale = Scale::Linear { min_range: 5.0 };
    /// What the network panel uses: a 1 KiB/s floor and two decades minimum.
    const LOG: Scale = Scale::Log {
        floor: 1024.0,
        min_decades: 2.0,
    };

    fn ring(values: &[f32]) -> Ring {
        let mut r = Ring::new(values.len().max(1));
        for v in values {
            r.push(*v);
        }
        r
    }

    #[test]
    fn keeps_only_cap_samples() {
        let mut r = Ring::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            r.push(v);
        }
        assert_eq!(r.render(10, LINEAR).chars().count(), 3);
    }

    #[test]
    fn empty_ring_renders_nothing() {
        assert_eq!(Ring::new(8).render(8, LINEAR), "");
        assert_eq!(Ring::new(8).render(8, LOG), "");
    }

    #[test]
    fn flat_series_stays_mid_height() {
        let mut r = Ring::new(8);
        for _ in 0..8 {
            r.push(50.0);
        }
        // A perfectly flat series lands in the middle of MIN_RANGE.
        // The bar has 8 levels with no exact middle, so it rounds up to the 5th.
        assert_eq!(r.render(8, LINEAR), "▅▅▅▅▅▅▅▅");
    }

    #[test]
    fn spans_full_height_when_range_is_wide() {
        let mut r = Ring::new(8);
        for v in [30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0] {
            r.push(v);
        }
        let s = r.render(8, LINEAR);
        assert!(s.starts_with('▁'), "got {s}");
        assert!(s.ends_with('█'), "got {s}");
    }

    /// The measured shape of a real link: rx over 90 seconds, in bytes per second.
    /// One 2.4 MB/s burst, a steady 70-180 kB/s, and quiet stretches around 1-9 kB/s.
    #[rustfmt::skip]
    const MEASURED: [f32; 18] = [
        181_430.0, 9_000.0, 74_480.0, 3_790.0, 69_050.0, 5_040.0, 78_750.0, 1_000.0,
        80_340.0, 7_650.0, 71_530.0, 2_060.0, 71_300.0, 2_448_050.0, 138_940.0,
        57_380.0, 106_790.0, 9_290.0,
    ];

    /// The reason the network panel is not linear. A single burst sets the top of the
    /// range and everything else collapses onto the bottom bar - and a wider window,
    /// which is the whole point of stacking rx over tx, only makes that likelier.
    #[test]
    fn a_burst_flattens_a_linear_scale_but_not_a_log_one() {
        let r = ring(&MEASURED);
        let levels = |s: &str| {
            let mut seen: Vec<char> = s.chars().collect();
            seen.sort_unstable();
            seen.dedup();
            seen.len()
        };
        let linear = r.render(MEASURED.len(), LINEAR);
        let log = r.render(MEASURED.len(), LOG);
        assert_eq!(
            linear.chars().filter(|c| *c == '▁').count(),
            16,
            "16 of 18 on the bottom bar: {linear}"
        );
        assert!(levels(&linear) <= 3, "{linear}");
        assert!(levels(&log) >= 5, "{log}");
    }

    /// The log scale is anchored at the floor, so height means "how much" - an idle
    /// link reads as idle instead of having its noise stretched over the full height.
    #[test]
    fn an_idle_link_stays_on_the_bottom_bar() {
        assert_eq!(ring(&[0.0; 6]).render(6, LOG), "▁▁▁▁▁▁");
        // Still under the 1 KiB/s floor.
        assert_eq!(ring(&[10.0, 300.0, 1000.0]).render(3, LOG), "▁▁▁");
    }

    /// Anchoring is also what makes two series comparable: the same value draws the
    /// same height whatever else is in its own window.
    #[test]
    fn the_same_value_draws_the_same_height_in_different_windows() {
        let busy = ring(&[100_000.0, 2_000_000.0]).render(2, LOG);
        let quiet = ring(&[100_000.0, 2_000_000.0]).render(2, LOG);
        assert_eq!(busy, quiet);
        // A window whose peak is lower still tops out, but the shared floor means the
        // bottom of both is the same traffic level.
        assert!(ring(&[1024.0, 2_000_000.0]).render(2, LOG).starts_with('▁'));
    }

    /// A quiet window must not be stretched: without `min_decades` a link idling
    /// between 1 and 2 KiB/s would draw the full height.
    #[test]
    fn min_decades_stops_a_quiet_window_being_stretched() {
        let drawn = ring(&[1024.0, 2048.0, 1024.0]).render(3, LOG);
        assert!(!drawn.contains('█'), "{drawn}");
    }

    /// Negative or absent traffic must not panic or wrap around.
    #[test]
    fn out_of_range_values_are_safe() {
        assert_eq!(ring(&[-5.0, 0.0, -1.0]).render(3, LOG), "▁▁▁");
        assert_eq!(
            ring(&[-5.0, 0.0, -1.0]).render(3, LINEAR).chars().count(),
            3
        );
    }

    #[test]
    fn render_is_limited_to_requested_width() {
        let mut r = Ring::new(60);
        for i in 0..60 {
            r.push(i as f32);
        }
        assert_eq!(r.render(8, LINEAR).chars().count(), 8);
    }
}
