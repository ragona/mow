//! Headless, deterministic gameplay core for Lawn Orbit.
//!
//! Nothing in this crate creates a window or GPU device. Rendering consumes the
//! immutable planet data and explicit dirty mowing tiles exposed here.

pub mod camera;
pub mod config;
pub mod cube_map;
pub mod flow;
pub mod input;
pub mod mowing;
pub mod physics;
pub mod planet;
pub mod profile;
pub mod run;
pub mod score;
pub mod simulation;
pub mod vehicle;

pub use config::{GameConfig, GeneratorConfig, JobConfig, VehicleTuning};
pub use mowing::{MowingField, MowingStamp};
pub use planet::{GeneratorVersion, Planet, PlanetGenerator, WorldSeed};
pub use run::{GameMode, RunState};

/// Fixed gameplay update frequency required by the specification.
pub const SIMULATION_HZ: u32 = 120;
/// Duration of one fixed simulation step.
pub const FIXED_DT: f32 = 1.0 / SIMULATION_HZ as f32;
