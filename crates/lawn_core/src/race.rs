//! Deterministic turf-race objectives and a surface-aware rival driver.

use crate::{
    mowing::MowingField,
    planet::{Planet, SpawnPoint},
};

mod reward;
mod rival;
use rival::RivalAi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaceOutcome {
    PlayerWon,
    RivalWon,
    Draw,
}

#[derive(Debug)]
pub struct RaceState {
    pub player_coverage: f64,
    pub rival_coverage: f64,
    pub outcome: Option<RaceOutcome>,
    /// Active simulation time when the race ended, retained during a victory lap.
    pub finished_seconds: Option<f32>,
    pub bumps: u32,
    pub(crate) ai: RivalAi,
    pub(crate) bump_cooldown: f32,
}

impl RaceState {
    #[must_use]
    pub fn new(planet: &Planet) -> Self {
        Self {
            player_coverage: 0.0,
            rival_coverage: 0.0,
            outcome: None,
            finished_seconds: None,
            bumps: 0,
            ai: RivalAi::new(planet),
            bump_cooldown: 0.0,
        }
    }

    pub(crate) fn update_score(&mut self, mowing: &MowingField, elapsed_seconds: f32) {
        if self.outcome.is_some() {
            return;
        }
        self.player_coverage = mowing.owned_coverage(1);
        self.rival_coverage = mowing.owned_coverage(2);
        self.outcome = if self.player_coverage > 0.5 {
            Some(RaceOutcome::PlayerWon)
        } else if self.rival_coverage > 0.5 {
            Some(RaceOutcome::RivalWon)
        } else if self.player_coverage + self.rival_coverage >= 1.0 - 1.0e-8 {
            Some(RaceOutcome::Draw)
        } else {
            None
        };
        if self.outcome.is_some() {
            self.finished_seconds = Some(elapsed_seconds);
        }
    }
}

/// Starts in the same hemisphere, with room to accelerate and meet over fresh grass.
#[must_use]
pub fn rival_spawn(planet: &Planet) -> SpawnPoint {
    let origin = planet.spawn.position.normalize();
    let right = planet.spawn.forward.cross(planet.spawn.up).normalize();
    let mut best = planet.spawn;
    let mut best_distance = 0.0_f32;
    for direction in [right, -right, planet.spawn.forward, -planet.spawn.forward] {
        let candidate = planet.nearest_safe_point((origin + direction * 0.52).normalize());
        let distance = candidate.position.distance(planet.spawn.position);
        if distance > best_distance && distance < planet.config.base_radius * 1.1 {
            best = candidate;
            best_distance = distance;
        }
    }
    best
}
