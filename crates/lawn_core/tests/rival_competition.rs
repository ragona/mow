//! Headless matches against active, deterministic player policies.
//!
//! Extended benchmark (run optimized; timing includes physics and mowing):
//! `cargo test -p lawn_core --release --test rival_competition benchmark_rival_competition -- --ignored --nocapture`
//! Optional environment: `RIVAL_BENCH_SEEDS=7,17,55`, `RIVAL_BENCH_SECONDS=180`,
//! `RIVAL_BENCH_RESOLUTION=128`, `RIVAL_BENCH_OPPONENT=greedy|legacy|both`,
//! `RIVAL_BENCH_TERRAIN=flat|rocky|both`.

use std::time::{Duration, Instant};

use glam::Vec3;
use lawn_core::{
    GameConfig, GameMode, GeneratorConfig, PlanetGenerator, RunState, SIMULATION_HZ, WorldSeed,
    camera::CameraRig,
    cube_map::{direction_to_cell, tangent_frame},
    input::InputSnapshot,
    planet::CURRENT_GENERATOR_VERSION,
    profile::AccessibilitySettings,
    race::RaceOutcome,
};

#[path = "support/legacy_rival.rs"]
mod legacy_rival;

#[derive(Clone, Copy, Debug)]
enum Opponent {
    Greedy,
    Legacy,
}

/// Deliberately simple active baseline: choose a fresh short strip, preserve
/// momentum when values are similar, and boost along clear productive strips.
/// It has no access to the rival's plan and does not use the live AI code.
#[derive(Default)]
struct GreedyPlayer {
    heading: Vec3,
    ticks_until_plan: u32,
    boost: bool,
}

impl GreedyPlayer {
    fn input(&mut self, run: &RunState) -> InputSnapshot {
        let vehicle = &run.vehicle.state;
        let origin = vehicle.transform.position.normalize();
        if self.ticks_until_plan == 0 {
            self.ticks_until_plan = (SIMULATION_HZ / 8).max(1);
            let forward = (vehicle.linear_velocity - origin * vehicle.linear_velocity.dot(origin))
                .try_normalize()
                .unwrap_or(vehicle.transform.forward);
            let right = forward.cross(origin).normalize_or(tangent_frame(origin).0);
            let mut best_score = f32::NEG_INFINITY;
            let mut best_fresh = 0.0;
            let mut best_clear = false;
            for candidate in 0..24 {
                let angle = candidate as f32 * std::f32::consts::TAU / 24.0;
                let heading = forward * angle.cos() + right * angle.sin();
                let side = heading.cross(origin).normalize();
                let mut fresh = 0.0;
                let mut clear = true;
                let mut score = 2.0 * heading.dot(forward);
                for step in 1..=8 {
                    let distance = step as f32 * 1.2;
                    let arc = distance / run.planet.config.base_radius;
                    let center = origin * arc.cos() + heading * arc.sin();
                    let terrain = run.planet.terrain_cell(center);
                    if !run.planet.is_mowable(center) || terrain.slope_radians > 0.85 {
                        score -= 12.0 / step as f32;
                        clear = false;
                        break;
                    }
                    for offset in [-0.7, 0.0, 0.7] {
                        let sample =
                            (center + side * (offset / run.planet.config.base_radius)).normalize();
                        let cell = direction_to_cell(sample, run.mowing.resolution());
                        if run.mowing.is_mowable(cell) && run.mowing.owner(cell) == 0 {
                            fresh += 1.0;
                            score += 1.0 - step as f32 * 0.035;
                        }
                    }
                }
                if score > best_score {
                    best_score = score;
                    self.heading = heading;
                    best_fresh = fresh;
                    best_clear = clear;
                }
            }
            self.boost = best_clear && best_fresh >= 12.0;
        }
        self.ticks_until_plan -= 1;
        let travel = (self.heading - origin * self.heading.dot(origin))
            .normalize_or(vehicle.transform.forward);
        camera_input(
            run,
            travel,
            1.0,
            self.boost && vehicle.boost_charge > 0.15,
            vehicle.stuck_seconds > 1.5,
        )
    }
}

fn camera_input(
    run: &RunState,
    travel: Vec3,
    throttle: f32,
    boost: bool,
    recover: bool,
) -> InputSnapshot {
    // Match HoverVehicle::tick's smoothed terrain-up frame. Player inputs are
    // camera-relative whereas the frozen rival's inputs are chassis-relative.
    let state = &run.vehicle.state;
    let terrain = run
        .planet
        .terrain_cell(state.transform.position.normalize());
    let up = state
        .transform
        .up
        .lerp(terrain.normal, 1.0 - (-9.0 * lawn_core::FIXED_DT).exp())
        .normalize();
    let screen_forward = (run.camera.state.up - up * run.camera.state.up.dot(up))
        .normalize_or(state.transform.forward);
    let screen_right = screen_forward.cross(up).normalize();
    InputSnapshot {
        accelerate: travel.dot(screen_forward).max(0.0) * throttle,
        brake_reverse: (-travel.dot(screen_forward)).max(0.0) * throttle,
        steer: travel.dot(screen_right) * throttle,
        boost_held: boost,
        recover_held: recover,
        ..InputSnapshot::default()
    }
}

fn make_run(seed: u64, rocky: bool, mirrored: bool, resolution: u32) -> RunState {
    let shipping = GameConfig::shipping().unwrap();
    let mut generator = GeneratorConfig {
        mowing_resolution: resolution,
        ..shipping.generator
    };
    if !rocky {
        generator.mountain_count_min = 0;
        generator.mountain_count_max = 0;
        generator.mowable_ratio_min = 1.0;
        generator.mowable_ratio_max = 1.0;
        generator.rolling_amplitude = 0.0;
    }
    let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, generator)
        .generate_with_roots(WorldSeed(seed), false)
        .unwrap();
    let settings = AccessibilitySettings::default();
    let mut run = RunState::new(
        planet,
        GameMode::TurfRace,
        shipping.vehicle,
        shipping.job,
        &settings,
        false,
    );
    if mirrored {
        // Swap complete vehicles, including their physics bodies, so both
        // policies get each initial position/heading without teleport artifacts.
        std::mem::swap(&mut run.vehicle, run.rival.as_mut().unwrap());
        run.camera = CameraRig::new(
            run.vehicle.state.transform,
            run.planet.config.base_radius,
            &settings,
        );
    }
    run
}

#[derive(Debug)]
struct MatchResult {
    outcome: Option<RaceOutcome>,
    seconds: f32,
    player: f64,
    rival: f64,
    rival_area_per_second: f64,
    // Approximation: movement on ticks with no new ownership, including cuts
    // still below the ownership threshold. Compare at the same resolution.
    rival_dead_distance_fraction: f32,
    rival_longest_drought_seconds: f32,
    rival_recoveries: u32,
    player_recoveries: u32,
    tick_mean: Duration,
    tick_p95: Duration,
    tick_p99: Duration,
    wall: Duration,
}

fn play_match(
    seed: u64,
    rocky: bool,
    mirrored: bool,
    resolution: u32,
    seconds: u32,
    opponent: Opponent,
) -> MatchResult {
    let mut run = make_run(seed, rocky, mirrored, resolution);
    let settings = AccessibilitySettings::default();
    let mut greedy = GreedyPlayer::default();
    let mut legacy = legacy_rival::LegacyRivalAi::new(&run.planet);
    let start = Instant::now();
    let mut tick_durations = Vec::with_capacity((seconds * SIMULATION_HZ) as usize);
    let mut previous_player = 0.0;
    let mut previous_rival = 0.0;
    let mut total_distance = 0.0;
    let mut dead_distance = 0.0;
    let mut last_gain_tick = 0;
    let mut longest_drought = 0;
    let mut recoveries = 0;
    for tick in 1..=seconds * SIMULATION_HZ {
        let rival = &run.rival.as_ref().unwrap().state;
        let previous_position = rival.transform.position;
        let input = match opponent {
            Opponent::Greedy => greedy.input(&run),
            Opponent::Legacy => {
                let input = legacy.drive(&run.planet, &run.mowing, &run.vehicle.state, rival);
                let state = &run.vehicle.state;
                let travel = state.transform.forward * (input.accelerate - input.brake_reverse)
                    + state
                        .transform
                        .forward
                        .cross(state.transform.up)
                        .normalize()
                        * input.steer;
                camera_input(
                    &run,
                    travel.normalize_or(state.transform.forward),
                    travel.length(),
                    input.boost_held,
                    input.recover_held,
                )
            }
        };
        run.clear_frame_events();
        let tick_start = Instant::now();
        run.tick(input, &settings);
        tick_durations.push(tick_start.elapsed());
        let race = run.race.as_ref().unwrap();
        assert!(race.player_coverage >= previous_player && race.rival_coverage >= previous_rival);
        assert!(race.player_coverage + race.rival_coverage <= 1.0 + 1.0e-10);
        if let Some(rival) = &run.rival {
            let distance = rival.state.transform.position.distance(previous_position);
            total_distance += distance;
            if race.rival_coverage <= previous_rival {
                dead_distance += distance;
            }
            recoveries = rival.state.recoveries;
        }
        if race.rival_coverage > previous_rival {
            last_gain_tick = tick;
        }
        longest_drought = longest_drought.max(tick - last_gain_tick);
        previous_player = race.player_coverage;
        previous_rival = race.rival_coverage;
        if race.outcome.is_some() {
            break;
        }
    }
    tick_durations.sort_unstable();
    let race = run.race.as_ref().unwrap();
    MatchResult {
        outcome: race.outcome,
        seconds: run.simulation_seconds,
        player: race.player_coverage,
        rival: race.rival_coverage,
        rival_area_per_second: race.rival_coverage * run.mowing.total_mowable_weight()
            / f64::from(run.simulation_seconds),
        rival_dead_distance_fraction: dead_distance / total_distance.max(0.001),
        rival_longest_drought_seconds: longest_drought as f32 / SIMULATION_HZ as f32,
        rival_recoveries: recoveries,
        player_recoveries: run.vehicle.state.recoveries,
        tick_mean: tick_durations.iter().copied().sum::<Duration>() / tick_durations.len() as u32,
        tick_p95: tick_durations[tick_durations.len() * 95 / 100],
        tick_p99: tick_durations[tick_durations.len() * 99 / 100],
        wall: start.elapsed(),
    }
}

fn report(seed: u64, rocky: bool, mirrored: bool, opponent: Opponent, result: &MatchResult) {
    eprintln!(
        "seed={seed} rocky={rocky} swapped={mirrored} opponent={opponent:?} t={:.1}s player={:.2}% rival={:.2}% outcome={:?} rival_area={:.2}m2/s dead_travel={:.1}% longest_drought={:.2}s recoveries={}/{} tick_mean/p95/p99={:.0}/{:.0}/{:.0}us wall={:.2}s",
        result.seconds,
        result.player * 100.0,
        result.rival * 100.0,
        result.outcome,
        result.rival_area_per_second,
        result.rival_dead_distance_fraction * 100.0,
        result.rival_longest_drought_seconds,
        result.rival_recoveries,
        result.player_recoveries,
        result.tick_mean.as_secs_f64() * 1.0e6,
        result.tick_p95.as_secs_f64() * 1.0e6,
        result.tick_p99.as_secs_f64() * 1.0e6,
        result.wall.as_secs_f64(),
    );
}

#[test]
fn rival_outscores_active_greedy_player_across_mirrored_worlds() {
    let mut wins = 0;
    let mut total_player = 0.0;
    let mut total_rival = 0.0;
    for (seed, rocky) in [(55, false), (7, true)] {
        for mirrored in [false, true] {
            let result = play_match(seed, rocky, mirrored, 128, 180, Opponent::Greedy);
            report(seed, rocky, mirrored, Opponent::Greedy, &result);
            assert!(
                result.player > 0.1,
                "the baseline must actively compete: {result:?}"
            );
            assert!(
                result.rival >= 0.45,
                "the rival must remain competitive: {result:?}"
            );
            assert!(result.rival_longest_drought_seconds < 10.0, "{result:?}");
            wins += u32::from(result.outcome == Some(RaceOutcome::RivalWon));
            total_player += result.player;
            total_rival += result.rival;
        }
    }
    // Evaluate the policy across both spawn assignments rather than requiring
    // it to win every near-even scramble for the final fraction of a percent.
    assert!(wins >= 3, "rival won only {wins} of 4 matches");
    assert!(
        total_rival >= total_player,
        "mean coverage: rival={} player={}",
        total_rival / 4.0,
        total_player / 4.0
    );
}

#[test]
fn rival_beats_frozen_legacy_driver_from_both_starts() {
    for mirrored in [false, true] {
        let result = play_match(17, true, mirrored, 128, 180, Opponent::Legacy);
        report(17, true, mirrored, Opponent::Legacy, &result);
        assert_eq!(result.outcome, Some(RaceOutcome::RivalWon), "{result:?}");
    }
}

#[test]
#[ignore = "manual multi-seed competition and whole-simulation timing benchmark"]
fn benchmark_rival_competition() {
    let seeds = std::env::var("RIVAL_BENCH_SEEDS").unwrap_or_else(|_| "7,17,55".into());
    let seeds: Vec<u64> = seeds
        .split(',')
        .map(|seed| seed.trim().parse().unwrap())
        .collect();
    let seconds = std::env::var("RIVAL_BENCH_SECONDS").map_or(180, |value| value.parse().unwrap());
    let resolution =
        std::env::var("RIVAL_BENCH_RESOLUTION").map_or(128, |value| value.parse().unwrap());
    let opponent = std::env::var("RIVAL_BENCH_OPPONENT").unwrap_or_else(|_| "both".into());
    let terrain = std::env::var("RIVAL_BENCH_TERRAIN").unwrap_or_else(|_| "both".into());
    assert!(seconds > 0);
    assert!(matches!(opponent.as_str(), "greedy" | "legacy" | "both"));
    assert!(matches!(terrain.as_str(), "flat" | "rocky" | "both"));
    let mut wins = 0;
    let mut losses = 0;
    let mut unfinished = 0;
    let mut draws = 0;
    let mut total_lead = 0.0;
    for (seed_index, seed) in seeds.into_iter().enumerate() {
        for rocky in [false, true] {
            // Featureless flat worlds are identical across generator seeds.
            if !rocky && seed_index > 0 {
                continue;
            }
            if (rocky && terrain == "flat") || (!rocky && terrain == "rocky") {
                continue;
            }
            for policy in [Opponent::Greedy, Opponent::Legacy] {
                if matches!(policy, Opponent::Greedy) && opponent == "legacy"
                    || matches!(policy, Opponent::Legacy) && opponent == "greedy"
                {
                    continue;
                }
                for mirrored in [false, true] {
                    let result = play_match(seed, rocky, mirrored, resolution, seconds, policy);
                    report(seed, rocky, mirrored, policy, &result);
                    match result.outcome {
                        Some(RaceOutcome::RivalWon) => wins += 1,
                        Some(RaceOutcome::PlayerWon) => losses += 1,
                        Some(RaceOutcome::Draw) => draws += 1,
                        None => unfinished += 1,
                    }
                    total_lead += result.rival - result.player;
                }
            }
        }
    }
    let matches = wins + losses + draws + unfinished;
    eprintln!(
        "SUMMARY matches={matches} rival_wins={wins} player_wins={losses} draws={draws} unfinished={unfinished} mean_rival_lead={:.2} percentage_points",
        total_lead * 100.0 / f64::from(matches),
    );
}
