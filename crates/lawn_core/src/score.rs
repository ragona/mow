//! Transparent run metrics and one-to-three-star evaluation.

use serde::{Deserialize, Serialize};

use crate::{
    config::JobConfig,
    planet::{GeneratorVersion, WorldSeed},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CollisionEvent {
    pub elapsed_seconds: f32,
    pub impulse: f32,
    pub position: [f32; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    pub elapsed_seconds: f32,
    pub coverage: f64,
    pub collisions: Vec<CollisionEvent>,
    pub recoveries: u32,
    pub distance_traveled: f32,
    pub estimated_ideal_distance: f32,
}

impl RunMetrics {
    #[must_use]
    pub fn substantial_collision_count(&self) -> u32 {
        self.collisions.len() as u32
    }

    #[must_use]
    pub fn efficiency(&self) -> f32 {
        if self.distance_traveled <= f32::EPSILON {
            0.0
        } else {
            (self.estimated_ideal_distance / self.distance_traveled).clamp(0.0, 1.0)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Results {
    pub generator_version: GeneratorVersion,
    pub world_seed: WorldSeed,
    pub completed: bool,
    pub stars: u8,
    pub metrics: RunMetrics,
}

impl Results {
    #[must_use]
    pub fn evaluate(
        generator_version: GeneratorVersion,
        world_seed: WorldSeed,
        metrics: RunMetrics,
        config: &JobConfig,
    ) -> Self {
        let completed = metrics.coverage >= config.completion_coverage;
        let collisions = metrics.substantial_collision_count();
        let stars = if !completed {
            0
        } else if metrics.elapsed_seconds <= config.three_star_seconds
            && metrics.coverage >= config.three_star_coverage
            && collisions == 0
            && metrics.recoveries == 0
        {
            3
        } else if metrics.elapsed_seconds <= config.two_star_seconds
            && collisions <= config.two_star_max_collisions
        {
            2
        } else {
            1
        };
        Self {
            generator_version,
            world_seed,
            completed,
            stars,
            metrics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_always_awards_at_least_one_star() {
        let config = JobConfig::default();
        let results = Results::evaluate(
            GeneratorVersion(1),
            WorldSeed(1),
            RunMetrics {
                elapsed_seconds: 10_000.0,
                coverage: 0.98,
                collisions: vec![CollisionEvent::default(); 20],
                recoveries: 3,
                ..RunMetrics::default()
            },
            &config,
        );
        assert_eq!(results.stars, 1);
    }

    #[test]
    fn strict_three_star_rules_are_separate_and_visible() {
        let results = Results::evaluate(
            GeneratorVersion(1),
            WorldSeed(1),
            RunMetrics {
                elapsed_seconds: 700.0,
                coverage: 0.997,
                ..RunMetrics::default()
            },
            &JobConfig::default(),
        );
        assert_eq!(results.stars, 3);
    }
}
