use serde::{Deserialize, Serialize};

/// Shipping gameplay data embedded into every binary from the editable RON
/// source at `config/game.ron`.
pub const SHIPPING_CONFIG_RON: &str = include_str!("../../../config/game.ron");

/// Data-driven planet generation and validation tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneratorConfig {
    pub base_radius: f32,
    pub terrain_resolution: u32,
    pub mowing_resolution: u32,
    pub patch_cells: u32,
    pub rolling_amplitude: f32,
    pub mountain_count_min: u8,
    pub mountain_count_max: u8,
    pub mountain_height_min: f32,
    pub mountain_height_max: f32,
    pub mowable_ratio_min: f32,
    pub mowable_ratio_max: f32,
    pub required_reachable_ratio: f32,
    pub mountain_separation_radians: f32,
    pub pass_clearance: f32,
    pub spawn_clearance: f32,
    pub spawn_max_slope_degrees: f32,
    pub grass_roots_per_square_meter: f32,
    pub grass_height_scale: f32,
    pub maximum_generation_attempts: u32,
    pub ideal_time_min_seconds: f32,
    pub ideal_time_max_seconds: f32,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            base_radius: 15.0,
            terrain_resolution: 64,
            mowing_resolution: 512,
            patch_cells: 8,
            rolling_amplitude: 0.65,
            mountain_count_min: 3,
            mountain_count_max: 7,
            mountain_height_min: 3.0,
            mountain_height_max: 5.5,
            mowable_ratio_min: 0.85,
            mowable_ratio_max: 0.95,
            required_reachable_ratio: 0.98,
            mountain_separation_radians: 0.58,
            pass_clearance: 4.4,
            spawn_clearance: 3.5,
            spawn_max_slope_degrees: 9.0,
            grass_roots_per_square_meter: 160.0,
            grass_height_scale: 2.25,
            maximum_generation_attempts: 8,
            ideal_time_min_seconds: 90.0,
            ideal_time_max_seconds: 480.0,
        }
    }
}

impl GeneratorConfig {
    /// Fast deterministic configuration intended for unit and fuzz tests.
    #[must_use]
    pub fn test_quality() -> Self {
        Self {
            terrain_resolution: 24,
            mowing_resolution: 64,
            grass_roots_per_square_meter: 0.0,
            ..Self::default()
        }
    }
}

/// Arcade vehicle tuning. Values are intentionally profile data, not literals in
/// the controller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VehicleTuning {
    pub hover_height: f32,
    pub car_length: f32,
    pub mower_width: f32,
    pub mower_length: f32,
    pub max_speed: f32,
    pub acceleration_time_90_percent: f32,
    pub braking_time_90_percent: f32,
    pub direction_change_time_90_percent: f32,
    pub boost_max_speed: f32,
    pub boost_acceleration_multiplier: f32,
    pub boost_capacity_seconds: f32,
    pub boost_recharge_delay: f32,
    pub boost_recharge_seconds: f32,
    pub recovery_hold_time: f32,
    pub automatic_recovery_delay: f32,
    pub surface_glue_acceleration: f32,
    pub surface_glue_damping: f32,
    pub cut_rate_per_second: f32,
}

impl Default for VehicleTuning {
    fn default() -> Self {
        Self {
            hover_height: 0.8,
            car_length: 2.4,
            mower_width: 2.2,
            mower_length: 0.8,
            max_speed: 12.0,
            acceleration_time_90_percent: 0.35,
            braking_time_90_percent: 0.2,
            direction_change_time_90_percent: 0.42,
            boost_max_speed: 28.0,
            boost_acceleration_multiplier: 3.0,
            boost_capacity_seconds: 1.5,
            boost_recharge_delay: 1.25,
            boost_recharge_seconds: 3.0,
            recovery_hold_time: 1.0,
            automatic_recovery_delay: 3.0,
            surface_glue_acceleration: 42.0,
            surface_glue_damping: 8.0,
            cut_rate_per_second: 8.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobConfig {
    pub completion_coverage: f64,
    pub locator_coverage: f64,
    pub two_star_seconds: f32,
    pub two_star_max_collisions: u32,
    pub three_star_seconds: f32,
    pub three_star_coverage: f64,
    pub recovery_time_penalty: f32,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            completion_coverage: 0.98,
            locator_coverage: 0.95,
            two_star_seconds: 5.0 * 60.0,
            two_star_max_collisions: 2,
            three_star_seconds: 3.0 * 60.0,
            three_star_coverage: 0.995,
            recovery_time_penalty: 3.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GameConfig {
    pub generator: GeneratorConfig,
    pub vehicle: VehicleTuning,
    pub job: JobConfig,
}

impl GameConfig {
    /// Parses the shipping tuning data bundled with the application.
    ///
    /// # Errors
    ///
    /// Returns a RON parse error if the checked-in configuration and Rust data
    /// schema diverge.
    pub fn shipping() -> Result<Self, ron::error::SpannedError> {
        ron::from_str(SHIPPING_CONFIG_RON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipping_tuning_is_valid_and_matches_documented_defaults() {
        let config = GameConfig::shipping().unwrap();
        assert_eq!(config, GameConfig::default());
        assert_eq!(config.generator.base_radius, 15.0);
        assert_eq!(config.generator.mowing_resolution, 512);
        assert_eq!(config.generator.grass_height_scale, 2.25);
        assert_eq!(config.vehicle.mower_width, 2.2);
        assert_eq!(config.vehicle.max_speed, 12.0);
        assert_eq!(config.vehicle.acceleration_time_90_percent, 0.35);
        assert_eq!(config.vehicle.braking_time_90_percent, 0.2);
        assert_eq!(config.vehicle.direction_change_time_90_percent, 0.42);
        assert_eq!(config.vehicle.boost_max_speed, 28.0);
        assert_eq!(config.vehicle.surface_glue_acceleration, 42.0);
        assert_eq!(config.job.completion_coverage, 0.98);
    }
}
