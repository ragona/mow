//! Fixed-rate simulation accumulator with render interpolation.

use std::time::Duration;

use crate::SIMULATION_HZ;

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
            fixed_seconds: 1.0 / f64::from(SIMULATION_HZ),
            maximum_catch_up_steps: 8,
            dropped_time_seconds: 0.0,
        }
    }
}

impl FixedStepClock {
    pub fn advance(&mut self, elapsed: Duration, mut tick: impl FnMut()) -> u32 {
        let elapsed = elapsed.as_secs_f64();
        let admitted = elapsed.min(0.25);
        self.dropped_time_seconds += elapsed - admitted;
        self.accumulator += admitted;
        let available = (self.accumulator / self.fixed_seconds).floor() as u32;
        let steps = available.min(self.maximum_catch_up_steps);
        for _ in 0..steps {
            tick();
        }
        self.accumulator -= f64::from(available) * self.fixed_seconds;
        if available > self.maximum_catch_up_steps {
            self.dropped_time_seconds += f64::from(available - steps) * self.fixed_seconds;
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

    #[test]
    fn dropped_time_is_accounted_for_without_leaving_a_stale_tick() {
        let mut clock = FixedStepClock::default();
        assert_eq!(clock.advance(Duration::from_secs(1), || {}), 8);
        assert!((clock.dropped_time_seconds - (1.0 - 8.0 / 120.0)).abs() < 1.0e-12);
        assert_eq!(clock.advance(Duration::ZERO, || panic!("stale tick")), 0);
        assert!(clock.interpolation_alpha() < 1.0);
    }
}
