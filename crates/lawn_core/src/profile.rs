//! Versioned local settings, accessibility options, and per-seed records.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    input::{Action, ControlMap},
    planet::{GeneratorVersion, WorldSeed},
    score::Results,
};

pub const PROFILE_VERSION: u32 = 5;

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
    pub camera_tilt_degrees: f32,
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
            camera_tilt_degrees: 12.0,
            field_of_view_degrees: 90.0,
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
pub struct Settings {
    pub accessibility: AccessibilitySettings,
    pub controls: ControlMap,
    pub quality: QualityPreset,
    pub grass_height_multiplier: f32,
    pub render_scale: f32,
    pub msaa_samples: u32,
    pub fullscreen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            accessibility: AccessibilitySettings::default(),
            controls: ControlMap::default(),
            quality: QualityPreset::Standard,
            grass_height_multiplier: 1.0,
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
        let previous_version = self.version;
        let a = &mut self.settings.accessibility;
        // Versions 1 through 3 used lens-specific defaults. Move those exact
        // defaults to the undistorted top-down camera while preserving custom FOVs.
        if previous_version < 4
            && ((a.field_of_view_degrees - 68.0).abs() < f32::EPSILON
                || (a.field_of_view_degrees - 110.0).abs() < f32::EPSILON)
        {
            a.field_of_view_degrees = 90.0;
        }
        // ToggleMower also accepts the short-lived ToggleFisheye action as a
        // deserialize alias. Neither action exists in current gameplay.
        self.settings.controls.bindings.remove(&Action::ToggleMower);
        // Older profiles can predate newly added actions. Preserve explicit
        // remaps and empty (unbound) lists while filling absent actions.
        for (action, bindings) in ControlMap::default().bindings {
            self.settings
                .controls
                .bindings
                .entry(action)
                .or_insert(bindings);
        }
        self.version = PROFILE_VERSION;
        let defaults = AccessibilitySettings::default();
        a.camera_shake = finite_clamp(a.camera_shake, 0.0, 1.0, defaults.camera_shake);
        a.camera_tilt_degrees = finite_clamp(
            a.camera_tilt_degrees,
            0.0,
            18.0,
            defaults.camera_tilt_degrees,
        );
        a.field_of_view_degrees = finite_clamp(
            a.field_of_view_degrees,
            60.0,
            120.0,
            defaults.field_of_view_degrees,
        );
        a.camera_follow_stiffness = finite_clamp(
            a.camera_follow_stiffness,
            1.0,
            20.0,
            defaults.camera_follow_stiffness,
        );
        a.steering_sensitivity = finite_clamp(
            a.steering_sensitivity,
            0.25,
            2.0,
            defaults.steering_sensitivity,
        );
        self.settings.grass_height_multiplier =
            finite_clamp(self.settings.grass_height_multiplier, 0.4, 1.6, 1.0);
        self.settings.render_scale = finite_clamp(self.settings.render_scale, 0.5, 1.0, 1.0);
        self.settings.msaa_samples = match self.settings.msaa_samples {
            1 | 2 | 4 => self.settings.msaa_samples,
            _ => 4,
        };
        let mut seen = BTreeSet::new();
        self.recent_seeds
            .retain(|key| seen.insert((key.version.0, key.seed.0)));
        self.recent_seeds.truncate(20);
        for record in self.records.values_mut() {
            record.best_rating = record.best_rating.min(3);
            record.best_time_seconds = record
                .best_time_seconds
                .filter(|value| value.is_finite() && *value >= 0.0);
            record.highest_coverage = if record.highest_coverage.is_finite() {
                record.highest_coverage.clamp(0.0, 1.0)
            } else {
                0.0
            };
            record.best_efficiency = finite_clamp(record.best_efficiency, 0.0, 1.0, 0.0);
        }
    }
}

fn finite_clamp(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::RunMetrics;

    #[test]
    fn nonfinite_settings_and_invalid_records_are_repaired() {
        let mut profile = Profile::default();
        let a = &mut profile.settings.accessibility;
        a.camera_shake = f32::NAN;
        a.camera_tilt_degrees = f32::INFINITY;
        a.field_of_view_degrees = f32::NEG_INFINITY;
        a.camera_follow_stiffness = f32::NAN;
        a.steering_sensitivity = f32::NAN;
        profile.settings.grass_height_multiplier = f32::NAN;
        profile.settings.render_scale = f32::NAN;
        let key = RecordKey {
            version: GeneratorVersion(1),
            seed: WorldSeed(7),
        };
        profile.recent_seeds = VecDeque::from([key; 30]);
        profile.records.insert(
            key,
            SeedRecord {
                best_rating: 255,
                best_time_seconds: Some(-5.0),
                highest_coverage: f64::NAN,
                best_efficiency: f32::INFINITY,
            },
        );
        profile.sanitize();
        assert_eq!(profile.settings, Settings::default());
        assert_eq!(profile.recent_seeds, VecDeque::from([key]));
        assert_eq!(
            profile.records[&key],
            SeedRecord {
                best_rating: 3,
                ..SeedRecord::default()
            }
        );
    }

    #[test]
    fn migration_adds_missing_actions_without_replacing_explicit_bindings() {
        let mut profile = Profile::default();
        profile.settings.controls.bindings.clear();
        profile
            .settings
            .controls
            .bindings
            .insert(Action::Boost, Vec::new());
        profile.sanitize();
        assert!(profile.settings.controls.bindings[&Action::Boost].is_empty());
        assert_eq!(
            profile.settings.controls.bindings[&Action::Accelerate],
            ControlMap::default().bindings[&Action::Accelerate]
        );
        assert_eq!(
            profile.settings.controls.bindings[&Action::CameraUp],
            ControlMap::default().bindings[&Action::CameraUp]
        );
    }

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
        assert_eq!(migrated.settings.grass_height_multiplier, 1.0);
        assert_eq!(migrated.settings.accessibility.field_of_view_degrees, 90.0);
    }

    #[test]
    fn grass_height_setting_is_clamped_to_the_supported_visual_range() {
        let mut too_short = Profile::default();
        too_short.settings.grass_height_multiplier = 0.1;
        too_short.sanitize();
        assert_eq!(too_short.settings.grass_height_multiplier, 0.4);

        let mut too_tall = Profile::default();
        too_tall.settings.grass_height_multiplier = 8.0;
        too_tall.sanitize();
        assert_eq!(too_tall.settings.grass_height_multiplier, 1.6);
    }

    #[test]
    fn legacy_camera_defaults_migrate_without_overwriting_custom_fov() {
        let mut old_default = Profile {
            version: 1,
            ..Profile::default()
        };
        old_default.settings.accessibility.field_of_view_degrees = 68.0;
        old_default.sanitize();
        assert_eq!(
            old_default.settings.accessibility.field_of_view_degrees,
            90.0
        );

        let mut wide_lens_default = Profile {
            version: 2,
            ..Profile::default()
        };
        wide_lens_default
            .settings
            .accessibility
            .field_of_view_degrees = 110.0;
        wide_lens_default.sanitize();
        assert_eq!(
            wide_lens_default
                .settings
                .accessibility
                .field_of_view_degrees,
            90.0
        );

        let mut custom = Profile {
            version: 1,
            ..Profile::default()
        };
        custom.settings.accessibility.field_of_view_degrees = 82.0;
        custom.sanitize();
        assert_eq!(custom.settings.accessibility.field_of_view_degrees, 82.0);
    }

    #[test]
    fn short_lived_fisheye_binding_is_removed_during_profile_migration() {
        let legacy = r#"(
            version: 3,
            settings: (
                accessibility: (field_of_view_degrees: 110.0),
                controls: (bindings: {ToggleFisheye: [Key("KeyF")]}),
            ),
        )"#;
        let mut profile: Profile = ron::from_str(legacy).unwrap();
        assert!(
            profile
                .settings
                .controls
                .bindings
                .contains_key(&Action::ToggleMower)
        );
        profile.sanitize();
        assert_eq!(profile.version, PROFILE_VERSION);
        assert_eq!(profile.settings.accessibility.field_of_view_degrees, 90.0);
        assert!(
            !profile
                .settings
                .controls
                .bindings
                .contains_key(&Action::ToggleMower)
        );
    }
}
