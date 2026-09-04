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
    /// Validates numeric domains before allocation, random sampling, or geometry.
    ///
    /// # Errors
    ///
    /// Returns a description of the first unsupported configuration value.
    pub fn validate(&self) -> Result<(), String> {
        if !(8..=512).contains(&self.terrain_resolution)
            || !(8..=2048).contains(&self.mowing_resolution)
        {
            return Err("terrain resolution must be 8..=512 and mowing resolution 8..=2048".into());
        }
        if self.patch_cells == 0 || self.patch_cells > self.terrain_resolution {
            return Err("patch cells must be between 1 and the terrain resolution".into());
        }
        for (name, value) in [
            ("base radius", self.base_radius),
            ("rolling amplitude", self.rolling_amplitude),
            ("minimum mountain height", self.mountain_height_min),
            ("maximum mountain height", self.mountain_height_max),
            ("minimum mowable ratio", self.mowable_ratio_min),
            ("maximum mowable ratio", self.mowable_ratio_max),
            ("required reachable ratio", self.required_reachable_ratio),
            ("mountain separation", self.mountain_separation_radians),
            ("pass clearance", self.pass_clearance),
            ("spawn clearance", self.spawn_clearance),
            ("maximum spawn slope", self.spawn_max_slope_degrees),
            ("grass root density", self.grass_roots_per_square_meter),
            ("grass height scale", self.grass_height_scale),
            ("minimum ideal time", self.ideal_time_min_seconds),
            ("maximum ideal time", self.ideal_time_max_seconds),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be finite and non-negative"));
            }
        }
        // Four noise octaves sum to less than 1.06. Keep the radial surface
        // strictly positive even at the lowest point of the rolling terrain.
        if self.base_radius <= self.rolling_amplitude * 1.06 || self.base_radius > 128.0 {
            return Err(
                "base radius must exceed 1.06 times rolling amplitude and be at most 128".into(),
            );
        }
        if self.mountain_count_min > self.mountain_count_max
            || self.mountain_height_min > self.mountain_height_max
            || self.mountain_height_max > self.base_radius
        {
            return Err(
                "mountain count/height ranges must be ordered, with height at most the base radius"
                    .into(),
            );
        }
        if self.mowable_ratio_min <= 0.0
            || self.mowable_ratio_min > self.mowable_ratio_max
            || self.mowable_ratio_max > 1.0
            || !(0.0..=1.0).contains(&self.required_reachable_ratio)
        {
            return Err(
                "mowable ratios must satisfy 0 < min <= max <= 1 and reachability must be 0..=1"
                    .into(),
            );
        }
        if self.mountain_separation_radians > std::f32::consts::PI
            || self.spawn_max_slope_degrees >= 90.0
            || self.pass_clearance <= 0.0
            || self.grass_height_scale <= 0.0
            || self.ideal_time_min_seconds > self.ideal_time_max_seconds
            || self.ideal_time_max_seconds <= 0.0
            || !(1..=1024).contains(&self.maximum_generation_attempts)
        {
            return Err("invalid separation, slope, clearance, grass scale, ideal-time range, or attempt limit".into());
        }
        Ok(())
    }

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

impl VehicleTuning {
    /// Validates the domains required by the hover and driving controllers.
    ///
    /// # Errors
    ///
    /// Returns the first unsupported numeric value or inconsistent speed limit.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("hover height", self.hover_height),
            ("car length", self.car_length),
            ("mower width", self.mower_width),
            ("mower length", self.mower_length),
            ("maximum speed", self.max_speed),
            (
                "acceleration response time",
                self.acceleration_time_90_percent,
            ),
            ("braking response time", self.braking_time_90_percent),
            (
                "direction-change response time",
                self.direction_change_time_90_percent,
            ),
            ("boost maximum speed", self.boost_max_speed),
            (
                "boost acceleration multiplier",
                self.boost_acceleration_multiplier,
            ),
            ("boost capacity", self.boost_capacity_seconds),
            ("boost recharge time", self.boost_recharge_seconds),
            ("recovery hold time", self.recovery_hold_time),
            ("automatic recovery delay", self.automatic_recovery_delay),
            ("surface glue acceleration", self.surface_glue_acceleration),
            ("cut rate", self.cut_rate_per_second),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("{name} must be finite and positive"));
            }
        }
        for (name, value) in [
            ("boost recharge delay", self.boost_recharge_delay),
            ("surface glue damping", self.surface_glue_damping),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be finite and non-negative"));
            }
        }
        if !(0.41..=2.8).contains(&self.hover_height) {
            return Err(
                "hover height must be 0.41..=2.8 to clear the body and remain within pad reach"
                    .into(),
            );
        }
        if self.boost_max_speed < self.max_speed {
            return Err("boost maximum speed must be at least the normal maximum speed".into());
        }
        Ok(())
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

impl JobConfig {
    /// Validates completion thresholds and scoring limits.
    ///
    /// # Errors
    ///
    /// Returns a description of a non-finite value or inconsistent threshold.
    pub fn validate(&self) -> Result<(), String> {
        if !self.completion_coverage.is_finite()
            || !self.locator_coverage.is_finite()
            || !self.three_star_coverage.is_finite()
            || self.completion_coverage <= 0.0
            || self.completion_coverage > 1.0
            || !(0.0..=self.completion_coverage).contains(&self.locator_coverage)
            || !(self.completion_coverage..=1.0).contains(&self.three_star_coverage)
        {
            return Err("coverage thresholds must satisfy 0 <= locator <= completion <= three-star <= 1, with completion positive".into());
        }
        if !self.two_star_seconds.is_finite()
            || !self.three_star_seconds.is_finite()
            || self.three_star_seconds <= 0.0
            || self.two_star_seconds < self.three_star_seconds
            || !self.recovery_time_penalty.is_finite()
            || self.recovery_time_penalty < 0.0
        {
            return Err("star time limits must be finite, positive, and ordered; recovery penalty must be finite and non-negative".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GameConfig {
    pub generator: GeneratorConfig,
    pub vehicle: VehicleTuning,
    pub job: JobConfig,
}

impl GameConfig {
    /// Checks configuration before it reaches allocation or simulation code.
    ///
    /// # Errors
    ///
    /// Returns the first invalid generator, vehicle, or job setting.
    pub fn validate(&self) -> Result<(), String> {
        self.generator.validate()?;
        self.vehicle.validate()?;
        self.job.validate()
    }

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
        config.validate().unwrap();
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

    #[test]
    fn invalid_controller_and_objective_settings_are_rejected() {
        let mut config = GameConfig::default();
        config.vehicle.boost_recharge_seconds = 0.0;
        assert!(config.validate().is_err());
        config.vehicle = VehicleTuning::default();
        config.vehicle.cut_rate_per_second = f32::NAN;
        assert!(config.validate().is_err());
        config.vehicle = VehicleTuning::default();
        config.job.locator_coverage = 0.99;
        assert!(config.validate().is_err());
        config.job = JobConfig::default();
        config.job.recovery_time_penalty = f32::INFINITY;
        assert!(config.validate().is_err());
    }
}
