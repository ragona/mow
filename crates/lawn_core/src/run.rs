//! Complete active-run composition: vehicle, mowing, tutorial, objectives, and scoring.

use std::cell::Cell;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::{
    FIXED_DT, SIMULATION_HZ,
    camera::CameraRig,
    config::{JobConfig, VehicleTuning},
    input::InputSnapshot,
    mowing::{MowingField, MowingStamp},
    planet::Planet,
    profile::AccessibilitySettings,
    score::{CollisionEvent, Results, RunMetrics},
    vehicle::{HoverVehicle, VehicleTickResult},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameMode {
    #[default]
    Standard,
    FreeMow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TutorialStage {
    #[default]
    Drive,
    NoticeMowing,
    Boost,
    Rock,
    WaitForLocator,
    Recovery,
    Locator,
    Submit,
    Complete,
}

impl TutorialStage {
    #[must_use]
    pub const fn prompt(self) -> Option<&'static str> {
        match self {
            Self::Drive => Some("Move in any direction to begin mowing"),
            Self::NoticeMowing => Some("The always-on deck shortens grass and raises coverage"),
            Self::Boost => Some("Hold Boost on a clear stretch"),
            Self::Rock => Some("Rock cannot be mowed—route around steep faces"),
            Self::WaitForLocator | Self::Complete => None,
            Self::Recovery => Some("Hold Recover if the mower becomes stuck"),
            Self::Locator => Some("The locator points toward the largest uncut patch"),
            Self::Submit => Some("Job complete: submit now or continue to 100%"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedStamp {
    pub elapsed_seconds: f32,
    pub from: Vec3,
    pub to: Vec3,
    pub width: f32,
}

#[derive(Clone, Debug, Default)]
pub struct RunRecorder {
    stamps: Vec<RecordedStamp>,
    last_recorded_seconds: f32,
}

impl RunRecorder {
    pub fn record(&mut self, stamp: RecordedStamp) {
        // A complete 20-minute run at 120 Hz can be large. Coalesce nearly
        // collinear adjacent stamps while retaining a faithful results replay.
        if let Some(last) = self.stamps.last_mut() {
            let start = last.from.normalize_or_zero();
            let join = last.to.normalize_or_zero();
            let end = stamp.to.normalize_or_zero();
            let old_normal = start.cross(join).try_normalize();
            let new_normal = join.cross(end).try_normalize();
            let same_arc = old_normal
                .zip(new_normal)
                .is_some_and(|(a, b)| a.dot(b) > 0.9995);
            let stationary = last.from.distance_squared(last.to) < 1.0e-8
                && stamp.from.distance_squared(stamp.to) < 1.0e-8;
            if last.to.distance_squared(stamp.from) < 1.0e-6
                && (last.width - stamp.width).abs() < 0.01
                && (0.0..=0.05).contains(&(stamp.elapsed_seconds - self.last_recorded_seconds))
                && start.dot(end) > 0.9995
                && (same_arc || stationary)
            {
                last.to = stamp.to;
                self.last_recorded_seconds = stamp.elapsed_seconds;
                return;
            }
        }
        self.last_recorded_seconds = stamp.elapsed_seconds;
        self.stamps.push(stamp);
    }

    #[must_use]
    pub fn stamps(&self) -> &[RecordedStamp] {
        &self.stamps
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunEvent {
    GrassCut { weight: f64 },
    RockScrape,
    SubstantialCollision { impulse: f32 },
    Recovered,
    CoverageMilestone(u8),
    CompletionAvailable,
    TutorialAdvanced(TutorialStage),
}

#[derive(Debug)]
pub struct RunState {
    pub planet: Planet,
    pub mode: GameMode,
    pub mowing: MowingField,
    pub vehicle: HoverVehicle,
    pub camera: CameraRig,
    pub metrics: RunMetrics,
    /// Always-running active simulation time used by visual epochs and the sandbox.
    pub simulation_seconds: f32,
    pub recorder: RunRecorder,
    pub tutorial_stage: TutorialStage,
    pub tutorial_stage_seconds: f32,
    pub tutorial_enabled: bool,
    pub completion_available: bool,
    pub paused: bool,
    pub active: bool,
    pub vehicle_tuning: VehicleTuning,
    pub job_config: JobConfig,
    previous_milestone: u8,
    recent_events: Vec<RunEvent>,
    simulation_ticks: u64,
    locator_cache: Cell<(f32, Option<Vec3>)>,
}

impl RunState {
    #[must_use]
    pub fn new(
        planet: Planet,
        mode: GameMode,
        vehicle_tuning: VehicleTuning,
        job_config: JobConfig,
        accessibility: &AccessibilitySettings,
        tutorial_enabled: bool,
    ) -> Self {
        let mowing = MowingField::from_planet(&planet);
        let vehicle = HoverVehicle::new(&planet, &vehicle_tuning);
        let camera = CameraRig::new(
            vehicle.state.transform,
            planet.config.base_radius,
            accessibility,
        );
        let ideal_distance =
            planet.validation.mowable_area as f32 / vehicle_tuning.mower_width * 1.18;
        Self {
            planet,
            mode,
            mowing,
            vehicle,
            camera,
            metrics: RunMetrics {
                estimated_ideal_distance: ideal_distance,
                ..RunMetrics::default()
            },
            simulation_seconds: 0.0,
            recorder: RunRecorder::default(),
            tutorial_stage: TutorialStage::Drive,
            tutorial_stage_seconds: 0.0,
            tutorial_enabled,
            completion_available: false,
            paused: false,
            active: true,
            vehicle_tuning,
            job_config,
            previous_milestone: 0,
            recent_events: Vec::new(),
            simulation_ticks: 0,
            locator_cache: Cell::new((f32::NEG_INFINITY, None)),
        }
    }

    pub fn tick(&mut self, input: InputSnapshot, accessibility: &AccessibilitySettings) {
        if !self.active || self.paused {
            return;
        }
        let input = input.sanitized();
        let tick = self.vehicle.tick(
            &self.planet,
            &self.vehicle_tuning,
            input,
            self.camera.state.up,
            accessibility.boost_enabled,
            FIXED_DT,
        );
        self.simulation_ticks += 1;
        self.simulation_seconds = (self.simulation_ticks as f64 / f64::from(SIMULATION_HZ)) as f32;
        if self.tutorial_enabled {
            self.tutorial_stage_seconds += FIXED_DT;
        }
        if self.mode == GameMode::Standard {
            self.metrics.elapsed_seconds = self.simulation_seconds
                + self.metrics.recoveries as f32 * self.job_config.recovery_time_penalty;
        }
        self.metrics.distance_traveled += tick.traveled_distance;
        self.record_vehicle_events(tick);
        self.cut_if_valid(tick);
        self.metrics.coverage = self.mowing.coverage();
        self.update_objectives();
        self.update_tutorial(input, accessibility);
        self.camera.update(
            &self.planet,
            self.vehicle.state.transform,
            self.vehicle.state.linear_velocity,
            input.camera_orbit,
            input.look_behind,
            input.recenter_pressed,
            accessibility,
            FIXED_DT,
        );
    }

    fn record_vehicle_events(&mut self, tick: VehicleTickResult) {
        if let Some(impulse) = tick.collision_impulse {
            self.metrics.collisions.push(CollisionEvent {
                elapsed_seconds: self.metrics.elapsed_seconds,
                impulse,
                position: self.vehicle.state.transform.position.to_array(),
            });
            self.recent_events
                .push(RunEvent::SubstantialCollision { impulse });
        }
        if tick.recovered {
            self.metrics.recoveries = self.vehicle.state.recoveries;
            if self.mode == GameMode::Standard {
                self.metrics.elapsed_seconds += self.job_config.recovery_time_penalty;
            }
            self.recent_events.push(RunEvent::Recovered);
        }
    }

    fn cut_if_valid(&mut self, tick: VehicleTickResult) {
        if tick.recovered || !self.vehicle.state.grounded {
            return;
        }
        let stamp = MowingStamp {
            from: tick.deck_from,
            to: tick.deck_to,
            comb_direction: self
                .vehicle
                .state
                .linear_velocity
                .try_normalize()
                .unwrap_or(self.vehicle.state.transform.forward),
            deck_width: self.vehicle_tuning.mower_width,
            cut_delta: self.vehicle_tuning.cut_rate_per_second * FIXED_DT,
            recent_epoch: ((self.simulation_seconds * 30.0) as u32 & 0xff) as u8,
        };
        let result = self.mowing.stamp(stamp);
        if result.newly_cut_weight > 0.0 {
            self.recent_events.push(RunEvent::GrassCut {
                weight: result.newly_cut_weight,
            });
        }
        if result.touched_rock {
            self.recent_events.push(RunEvent::RockScrape);
        }
        if result.touched_grass_cells > 0 {
            self.recorder.record(RecordedStamp {
                elapsed_seconds: self.simulation_seconds,
                from: tick.deck_from,
                to: tick.deck_to,
                width: self.vehicle_tuning.mower_width,
            });
        }
    }

    fn update_objectives(&mut self) {
        let coverage_percent = (self.mowing.coverage() * 100.0).floor() as u8;
        let milestone = match coverage_percent {
            100 => 100,
            95.. => 95,
            75.. => 75,
            50.. => 50,
            25.. => 25,
            _ => 0,
        };
        if milestone > self.previous_milestone {
            self.previous_milestone = milestone;
            self.recent_events
                .push(RunEvent::CoverageMilestone(milestone));
        }
        if !self.completion_available
            && self.mowing.coverage() >= self.job_config.completion_coverage
        {
            self.completion_available = true;
            self.recent_events.push(RunEvent::CompletionAvailable);
        }
    }

    fn update_tutorial(&mut self, input: InputSnapshot, accessibility: &AccessibilitySettings) {
        if !self.tutorial_enabled || self.tutorial_stage == TutorialStage::Complete {
            return;
        }
        let next = match self.tutorial_stage {
            TutorialStage::Drive
                if input.steer.hypot(input.accelerate - input.brake_reverse) > 0.1 =>
            {
                Some(TutorialStage::NoticeMowing)
            }
            TutorialStage::NoticeMowing
                if self.mowing.coverage() > 0.0005 && self.tutorial_stage_seconds >= 1.2 =>
            {
                Some(TutorialStage::Boost)
            }
            TutorialStage::Boost
                if self.vehicle.state.boost_active || !accessibility.boost_enabled =>
            {
                Some(if self.planet.validation.mowable_ratio == 1.0 {
                    TutorialStage::WaitForLocator
                } else {
                    TutorialStage::Rock
                })
            }
            TutorialStage::Rock | TutorialStage::WaitForLocator
                if self.mowing.coverage() >= self.job_config.locator_coverage =>
            {
                Some(TutorialStage::Locator)
            }
            TutorialStage::Rock | TutorialStage::WaitForLocator
                if self.vehicle.state.stuck_seconds >= 1.0 =>
            {
                Some(TutorialStage::Recovery)
            }
            TutorialStage::Rock
                if self.recent_events.iter().any(|event| {
                    matches!(
                        event,
                        RunEvent::RockScrape | RunEvent::SubstantialCollision { .. }
                    )
                }) =>
            {
                Some(TutorialStage::WaitForLocator)
            }
            TutorialStage::Recovery if self.metrics.recoveries > 0 => {
                Some(TutorialStage::WaitForLocator)
            }
            TutorialStage::Locator if self.completion_available => {
                Some(if self.mode == GameMode::FreeMow {
                    TutorialStage::Complete
                } else {
                    TutorialStage::Submit
                })
            }
            _ => None,
        };
        if let Some(next) = next {
            self.tutorial_stage = next;
            self.tutorial_stage_seconds = 0.0;
            self.recent_events.push(RunEvent::TutorialAdvanced(next));
        }
    }

    #[must_use]
    pub fn locator_direction(&self) -> Option<Vec3> {
        let coverage = self.mowing.coverage();
        if coverage < self.job_config.locator_coverage || coverage >= 1.0 {
            return None;
        }
        let (updated_seconds, direction) = self.locator_cache.get();
        if self.simulation_seconds - updated_seconds < 0.25 {
            return direction;
        }
        let direction = self.mowing.largest_uncut_direction();
        self.locator_cache.set((self.simulation_seconds, direction));
        direction
    }

    #[must_use]
    pub fn events(&self) -> &[RunEvent] {
        &self.recent_events
    }

    /// Called once before a displayed frame's fixed ticks. Events from every
    /// catch-up tick are retained for particles and controller rumble.
    pub fn clear_frame_events(&mut self) {
        self.recent_events.clear();
    }

    pub fn submit(&mut self) -> Option<Results> {
        if !self.active || self.mode == GameMode::FreeMow || !self.completion_available {
            return None;
        }
        self.active = false;
        self.metrics.coverage = self.mowing.coverage();
        Some(Results::evaluate(
            self.planet.generator_version,
            self.planet.world_seed,
            self.metrics.clone(),
            &self.job_config,
        ))
    }

    /// Reset every mutable run field while retaining the generated planet/seed.
    pub fn restart(&mut self, accessibility: &AccessibilitySettings) {
        self.mowing = MowingField::from_planet(&self.planet);
        self.vehicle = HoverVehicle::new(&self.planet, &self.vehicle_tuning);
        self.camera = CameraRig::new(
            self.vehicle.state.transform,
            self.planet.config.base_radius,
            accessibility,
        );
        self.metrics = RunMetrics {
            estimated_ideal_distance: self.planet.validation.mowable_area as f32
                / self.vehicle_tuning.mower_width
                * 1.18,
            ..RunMetrics::default()
        };
        self.simulation_seconds = 0.0;
        self.simulation_ticks = 0;
        self.locator_cache.set((f32::NEG_INFINITY, None));
        self.recorder = RunRecorder::default();
        self.tutorial_stage = TutorialStage::Drive;
        self.tutorial_stage_seconds = 0.0;
        self.completion_available = false;
        self.paused = false;
        self.active = true;
        self.previous_milestone = 0;
        self.recent_events.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION,
        simulation::FixedStepClock,
    };

    fn run(mode: GameMode) -> RunState {
        let planet =
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, GeneratorConfig::test_quality())
                .generate_with_roots(WorldSeed(55), false)
                .unwrap();
        RunState::new(
            planet,
            mode,
            VehicleTuning::default(),
            JobConfig::default(),
            &AccessibilitySettings::default(),
            false,
        )
    }

    #[test]
    fn free_mow_does_not_advance_timer() {
        let mut run = run(GameMode::FreeMow);
        for _ in 0..120 {
            run.tick(
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                &AccessibilitySettings::default(),
            );
        }
        assert_eq!(run.metrics.elapsed_seconds, 0.0);
        assert!(run.metrics.distance_traveled > 0.0);
    }

    #[test]
    fn recovering_never_cuts_a_path_to_the_recovery_point() {
        let mut run = run(GameMode::Standard);
        let before = run.mowing.packed_cells().to_vec();
        run.cut_if_valid(VehicleTickResult {
            recovered: true,
            deck_from: Vec3::X * 15.0,
            deck_to: Vec3::Y * 15.0,
            ..VehicleTickResult::default()
        });
        assert_eq!(run.mowing.packed_cells(), before);
        assert!(run.recorder.stamps().is_empty());
    }

    #[test]
    fn run_clock_uses_tick_count_without_long_session_accumulation_error() {
        let mut run = run(GameMode::Standard);
        run.simulation_ticks = 120 * 1200;
        run.metrics.recoveries = 2;
        run.tick(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.simulation_seconds, (1200.0_f64 + 1.0 / 120.0) as f32);
        assert_eq!(run.metrics.elapsed_seconds, run.simulation_seconds + 6.0);
    }

    #[test]
    fn completed_run_can_only_be_submitted_once() {
        let mut run = run(GameMode::Standard);
        run.completion_available = true;
        assert!(run.submit().is_some());
        assert!(run.submit().is_none());
    }

    #[test]
    fn recorder_keeps_turns_and_long_arcs_instead_of_collapsing_the_route() {
        let mut recorder = RunRecorder::default();
        let mut previous = Vec3::X * 15.0;
        for tick in 1..=400 {
            let angle = tick as f32 * 0.001;
            let next = Vec3::new(angle.cos(), angle.sin(), 0.0) * 15.0;
            recorder.record(RecordedStamp {
                elapsed_seconds: tick as f32 / 120.0,
                from: previous,
                to: next,
                width: 2.2,
            });
            previous = next;
        }
        assert!(recorder.stamps().len() > 10);
        assert!(recorder.stamps().len() < 100);
        let count = recorder.stamps().len();
        recorder.record(RecordedStamp {
            elapsed_seconds: 401.0 / 120.0,
            from: previous,
            to: (previous + Vec3::Z * 0.01).normalize() * 15.0,
            width: 2.2,
        });
        assert_eq!(recorder.stamps().len(), count + 1);
    }

    #[test]
    fn recorder_preserves_an_out_and_back_path() {
        let mut recorder = RunRecorder::default();
        let from = Vec3::X * 15.0;
        let to = (Vec3::X + Vec3::Y * 0.01).normalize() * 15.0;
        recorder.record(RecordedStamp {
            elapsed_seconds: 0.0,
            from,
            to,
            width: 2.2,
        });
        recorder.record(RecordedStamp {
            elapsed_seconds: FIXED_DT,
            from: to,
            to: from,
            width: 2.2,
        });
        assert_eq!(recorder.stamps().len(), 2);
        assert_eq!(recorder.stamps()[0].to, to);
        assert_eq!(recorder.stamps()[1].to, from);
    }

    #[test]
    fn sandbox_tutorial_finishes_without_a_submission_step() {
        let mut run = run(GameMode::FreeMow);
        run.tutorial_enabled = true;
        run.tutorial_stage = TutorialStage::Locator;
        run.completion_available = true;

        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());

        assert_eq!(run.tutorial_stage, TutorialStage::Complete);
    }

    #[test]
    fn restart_resets_all_mutable_progress_and_keeps_seed() {
        let mut run = run(GameMode::Standard);
        let seed = run.planet.world_seed;
        for _ in 0..60 {
            run.tick(
                InputSnapshot {
                    accelerate: 1.0,
                    ..InputSnapshot::default()
                },
                &AccessibilitySettings::default(),
            );
        }
        assert!(run.metrics.elapsed_seconds > 0.0);
        run.restart(&AccessibilitySettings::default());
        assert_eq!(run.planet.world_seed, seed);
        assert_eq!(run.metrics.elapsed_seconds, 0.0);
        assert_eq!(run.mowing.coverage(), 0.0);
        assert_eq!(run.metrics.collisions.len(), 0);
    }

    #[test]
    fn simulation_and_mowing_are_render_rate_independent() {
        let mut reference: Option<(Vec<crate::mowing::PackedMowingCell>, f64, Vec3, f32)> = None;
        for frames_per_second in [30_u32, 60, 120, 240] {
            let mut run = run(GameMode::Standard);
            let mut clock = FixedStepClock::default();
            let mut ticks = 0_u32;
            while ticks < 600 {
                clock.advance(
                    Duration::from_secs_f64(1.0 / f64::from(frames_per_second)),
                    || {
                        if ticks < 600 {
                            run.tick(
                                InputSnapshot {
                                    accelerate: 0.82,
                                    steer: 0.11,
                                    ..InputSnapshot::default()
                                },
                                &AccessibilitySettings::default(),
                            );
                            ticks += 1;
                        }
                    },
                );
            }
            let outcome = (
                run.mowing.packed_cells().to_vec(),
                run.mowing.coverage(),
                run.vehicle.state.transform.position,
                run.metrics.distance_traveled,
            );
            if let Some(reference) = &reference {
                assert_eq!(
                    outcome.0, reference.0,
                    "mowing differed at {frames_per_second} FPS"
                );
                assert_eq!(outcome.1, reference.1);
                assert!(outcome.2.distance(reference.2) < 1.0e-5);
                assert!((outcome.3 - reference.3).abs() < 1.0e-5);
            } else {
                reference = Some(outcome);
            }
        }
    }

    #[test]
    fn contextual_tutorial_prompts_cannot_be_skipped_within_one_render_frame() {
        let mut run = run(GameMode::Standard);
        run.tutorial_enabled = true;
        run.tutorial_stage = TutorialStage::NoticeMowing;
        run.mowing.stamp(MowingStamp {
            from: run.planet.spawn.position,
            to: run.planet.spawn.position,
            comb_direction: run.planet.spawn.forward,
            deck_width: 4.0,
            cut_delta: 1.0,
            recent_epoch: 1,
        });
        assert!(run.mowing.coverage() > 0.0005);
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::NoticeMowing);
        run.tutorial_stage_seconds = 1.2;
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::Boost);

        run.tutorial_stage = TutorialStage::Locator;
        run.completion_available = false;
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::Locator);
        run.completion_available = true;
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::Submit);
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::Submit);
    }

    #[test]
    fn recovery_instruction_is_an_optional_stuck_branch() {
        let mut run = run(GameMode::Standard);
        run.tutorial_enabled = true;
        run.tutorial_stage = TutorialStage::WaitForLocator;
        run.vehicle.state.stuck_seconds = 1.1;
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::Recovery);
        run.metrics.recoveries = 1;
        run.update_tutorial(InputSnapshot::default(), &AccessibilitySettings::default());
        assert_eq!(run.tutorial_stage, TutorialStage::WaitForLocator);
    }
}
