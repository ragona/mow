//! Reproducible CPU workload; run with `cargo run --release -p lawn_tools --example performance`.
//! Timings are observations, not assertions: compare on the same idle machine.

use std::{hint::black_box, time::Instant};

use anyhow::Result;
use lawn_core::{
    GameConfig, GameMode, PlanetGenerator, RunState, WorldSeed, input::InputSnapshot,
    mowing::PackedMowingCell, profile::AccessibilitySettings,
};
use serde_json::json;

fn main() -> Result<()> {
    let config = GameConfig::shipping()?;
    let settings = AccessibilitySettings::default();
    let start = Instant::now();
    let planet = PlanetGenerator::default().generate_with_roots(WorldSeed(42), false)?;
    let generation_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let mut run = RunState::new(
        planet,
        GameMode::FreeMow,
        config.vehicle,
        config.job,
        &settings,
        false,
    );
    let preparation_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut tick_us = Vec::with_capacity(3600);
    for tick in 0..3600 {
        let input = InputSnapshot {
            accelerate: 1.0,
            steer: (tick as f32 / 360.0).sin() * 0.7,
            boost_held: tick % 600 < 120,
            ..InputSnapshot::default()
        };
        let start = Instant::now();
        run.tick(input, &settings);
        black_box(run.events());
        run.clear_frame_events();
        // Exercise the same upload extraction frequency as a 60 Hz renderer.
        if tick % 2 == 0 {
            black_box(run.mowing.take_dirty_tiles());
        }
        tick_us.push(start.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let total_tick_ms = tick_us.iter().sum::<f64>() / 1000.0;
    tick_us.sort_by(f64::total_cmp);
    let coverage_after_drive = run.mowing.coverage();

    let mut snapshot = run.mowing.snapshot();
    let cut_end = snapshot.cells.len() * 95 / 100;
    snapshot.cells[..cut_end].fill(PackedMowingCell(255));
    run.mowing.restore(&snapshot)?;
    let start = Instant::now();
    black_box(run.mowing.largest_uncut_direction());
    let cold_locator_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    for _ in 0..10 {
        black_box(run.mowing.largest_uncut_direction());
    }
    let cached_locator_ms = start.elapsed().as_secs_f64() * 1000.0 / 10.0;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "seed": WorldSeed(42).to_string(),
            "generator_version": run.planet.generator_version.0,
            "mowing_resolution": run.mowing.resolution(),
            "generation_without_roots_ms": generation_ms,
            "gameplay_preparation_ms": preparation_ms,
            "simulation_ticks": tick_us.len(),
            "tick_us": { "median": tick_us[tick_us.len() / 2], "p95": tick_us[tick_us.len() * 95 / 100],
                "maximum": tick_us.last(), "total_ms": total_tick_ms },
            "coverage_after_drive": coverage_after_drive,
            "locator_ms": { "cold": cold_locator_ms, "unchanged_mean": cached_locator_ms },
        }))?
    );
    Ok(())
}
