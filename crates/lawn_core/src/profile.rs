//! Versioned local settings, accessibility options, and per-seed records.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    input::ControlMap,
    planet::{GeneratorVersion, WorldSeed},
    score::Results,
};

pub const PROFILE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityPreset {
    Low,
    Standard,
    High,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessibilitySettings {
    pub camera_shake: f32,
    pub field_of_view_degrees: f32,
    pub camera_follow_stiffness: f32,
    pub fixed_horizon: bool,
    pub steering_sensitivity: f32,
    pub invert_steering: bool,
    pub invert_camera_y: bool,
    pub boost_enabled: bool,
    pub high_contrast_grass: bool,
    pub reduced_particles: bool,
    pub enlarged_locator: bool,
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            camera_shake: 0.35,
            field_of_view_degrees: 68.0,
            camera_follow_stiffness: 9.0,
            fixed_horizon: false,
            steering_sensitivity: 1.0,
            invert_steering: false,
            invert_camera_y: false,
            boost_enabled: true,
            high_contrast_grass: false,
            reduced_particles: false,
            enlarged_locator: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub master: f32,
    pub music: f32,
    pub effects: f32,
    pub ambience: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master: 0.8,
            music: 0.55,
            effects: 0.85,
            ambience: 0.65,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub accessibility: AccessibilitySettings,
    pub audio: AudioSettings,
    pub controls: ControlMap,
    pub quality: QualityPreset,
    pub render_scale: f32,
    pub msaa_samples: u32,
    pub fullscreen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            accessibility: AccessibilitySettings::default(),
            audio: AudioSettings::default(),
            controls: ControlMap::default(),
            quality: QualityPreset::Standard,
            render_scale: 1.0,
            msaa_samples: 4,
            fullscreen: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordKey {
    pub version: GeneratorVersion,
    pub seed: WorldSeed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedRecord {
    pub best_rating: u8,
    pub best_time_seconds: Option<f32>,
    pub highest_coverage: f64,
    pub best_efficiency: f32,
}

impl SeedRecord {
    pub fn include(&mut self, result: &Results) {
        self.best_rating = self.best_rating.max(result.stars);
        self.highest_coverage = self.highest_coverage.max(result.metrics.coverage);
        self.best_efficiency = self.best_efficiency.max(result.metrics.efficiency());
        if result.completed {
            self.best_time_seconds = Some(
                self.best_time_seconds
                    .map_or(result.metrics.elapsed_seconds, |old| {
                        old.min(result.metrics.elapsed_seconds)
                    }),
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub version: u32,
    pub settings: Settings,
    pub tutorial_completed: bool,
    pub tutorial_reset_requested: bool,
    pub unlocked_content: BTreeSet<String>,
    pub recent_seeds: VecDeque<RecordKey>,
    pub favorite_seeds: BTreeSet<(u32, u64)>,
    pub records: HashMap<RecordKey, SeedRecord>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            settings: Settings::default(),
            tutorial_completed: false,
            tutorial_reset_requested: false,
            unlocked_content: BTreeSet::from(["standard_job".into(), "free_mow".into()]),
            recent_seeds: VecDeque::new(),
            favorite_seeds: BTreeSet::new(),
            records: HashMap::new(),
        }
    }
}

impl Profile {
    pub fn record_seed(&mut self, version: GeneratorVersion, seed: WorldSeed) {
        let key = RecordKey { version, seed };
        self.recent_seeds.retain(|old| *old != key);
        self.recent_seeds.push_front(key);
        self.recent_seeds.truncate(20);
    }

    pub fn toggle_favorite(&mut self, version: GeneratorVersion, seed: WorldSeed) -> bool {
        let key = (version.0, seed.0);
        if self.favorite_seeds.remove(&key) {
            false
        } else {
            self.favorite_seeds.insert(key);
            true
        }
    }

    pub fn record_result(&mut self, result: &Results) {
        let key = RecordKey {
            version: result.generator_version,
            seed: result.world_seed,
        };
        self.records.entry(key).or_default().include(result);
        self.record_seed(result.generator_version, result.world_seed);
    }

    pub fn sanitize(&mut self) {
        self.version = PROFILE_VERSION;
        let a = &mut self.settings.accessibility;
        a.camera_shake = a.camera_shake.clamp(0.0, 1.0);
        a.field_of_view_degrees = a.field_of_view_degrees.clamp(50.0, 95.0);
        a.camera_follow_stiffness = a.camera_follow_stiffness.clamp(1.0, 20.0);
        a.steering_sensitivity = a.steering_sensitivity.clamp(0.25, 2.0);
        self.settings.render_scale = self.settings.render_scale.clamp(0.5, 1.0);
        self.settings.msaa_samples = match self.settings.msaa_samples {
            1 | 2 | 4 => self.settings.msaa_samples,
            _ => 4,
        };
        for volume in [
            &mut self.settings.audio.master,
            &mut self.settings.audio.music,
            &mut self.settings.audio.effects,
            &mut self.settings.audio.ambience,
        ] {
            *volume = volume.clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::RunMetrics;

    #[test]
    fn records_are_seed_specific_and_improve_monotonically() {
        let mut profile = Profile::default();
        let result = Results {
            generator_version: GeneratorVersion(1),
            world_seed: WorldSeed(9),
            completed: true,
            stars: 2,
            metrics: RunMetrics {
                elapsed_seconds: 600.0,
                coverage: 0.99,
                distance_traveled: 100.0,
                estimated_ideal_distance: 80.0,
                ..RunMetrics::default()
            },
        };
        profile.record_result(&result);
        let mut slower = result.clone();
        slower.metrics.elapsed_seconds = 800.0;
        profile.record_result(&slower);
        let record = profile
            .records
            .get(&RecordKey {
                version: GeneratorVersion(1),
                seed: WorldSeed(9),
            })
            .unwrap();
        assert_eq!(record.best_time_seconds, Some(600.0));
        assert_eq!(profile.recent_seeds.len(), 1);
    }

    #[test]
    fn profile_round_trips_and_missing_new_fields_use_defaults() {
        let profile = Profile::default();
        let encoded = serde_json::to_string(&profile).unwrap();
        let decoded: Profile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, profile);

        let legacy = r#"{
            "version": 0,
            "settings": {},
            "tutorial_completed": false,
            "unlocked_content": [],
            "recent_seeds": [],
            "favorite_seeds": [],
            "records": {}
        }"#;
        let mut migrated: Profile = serde_json::from_str(legacy).unwrap();
        migrated.sanitize();
        assert_eq!(migrated.version, PROFILE_VERSION);
        assert_eq!(migrated.settings.msaa_samples, 4);
    }
}
