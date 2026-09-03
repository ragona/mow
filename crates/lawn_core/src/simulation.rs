//! Fixed-rate simulation accumulator with render interpolation.

use std::time::Duration;

use crate::FIXED_DT;

#[derive(Clone, Debug)]
pub struct FixedStepClock {
    accumulator: f64,
    fixed_seconds: f64,
    maximum_catch_up_steps: u32,
    pub dropped_time_seconds: f64,
}

impl Default for FixedStepClock {
    fn default() -> Self {
        Self {
            accumulator: 0.0,
            fixed_seconds: f64::from(FIXED_DT),
            maximum_catch_up_steps: 8,
            dropped_time_seconds: 0.0,
        }
    }
}

impl FixedStepClock {
    pub fn advance(&mut self, elapsed: Duration, mut tick: impl FnMut()) -> u32 {
        self.accumulator += elapsed.as_secs_f64().min(0.25);
        let available = (self.accumulator / self.fixed_seconds).floor() as u32;
        let steps = available.min(self.maximum_catch_up_steps);
        for _ in 0..steps {
            tick();
        }
        self.accumulator -= f64::from(steps) * self.fixed_seconds;
        if available > self.maximum_catch_up_steps {
            let dropped = self.accumulator - self.fixed_seconds;
            if dropped > 0.0 {
                self.dropped_time_seconds += dropped;
                self.accumulator = self.fixed_seconds;
            }
        }
        steps
    }

    #[must_use]
    pub fn interpolation_alpha(&self) -> f32 {
        (self.accumulator / self.fixed_seconds).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_rate_does_not_change_tick_count() {
        for fps in [30, 60, 120, 240] {
            let mut clock = FixedStepClock::default();
            let mut ticks: i32 = 0;
            for _ in 0..fps * 5 {
                clock.advance(Duration::from_secs_f64(1.0 / f64::from(fps)), || ticks += 1);
            }
            assert!((ticks - 600).abs() <= 1, "fps={fps}, ticks={ticks}");
        }
    }

    #[test]
    fn catch_up_is_bounded() {
        let mut clock = FixedStepClock::default();
        let mut ticks = 0;
        let steps = clock.advance(Duration::from_secs(1), || ticks += 1);
        assert_eq!(steps, 8);
        assert_eq!(ticks, 8);
        assert!(clock.dropped_time_seconds > 0.0);
    }
}
