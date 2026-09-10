//! Public-API regressions for two-mower competition, without a renderer/window.

use std::time::Duration;

use glam::Vec3;
use lawn_core::{
    GameConfig, GameMode, GeneratorConfig, PlanetGenerator, RunState, SIMULATION_HZ, WorldSeed,
    input::InputSnapshot,
    mowing::{MowingStamp, PackedMowingCell},
    planet::CURRENT_GENERATOR_VERSION,
    profile::AccessibilitySettings,
    race::RaceOutcome,
    run::{RecordedStamp, RunEvent},
    score::RunMetrics,
    simulation::FixedStepClock,
    vehicle::{HoverVehicle, VehicleState},
};

fn race_run(resolution: u32, rocky: bool) -> RunState {
    let shipping = GameConfig::shipping().unwrap();
    let generator = GeneratorConfig {
        mowing_resolution: resolution,
        ..shipping.generator
    };
    let generator = if rocky {
        generator
    } else {
        GeneratorConfig {
            mountain_count_min: 0,
            mountain_count_max: 0,
            mowable_ratio_min: 1.0,
            mowable_ratio_max: 1.0,
            rolling_amplitude: 0.0,
            ..generator
        }
    };
    let planet = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, generator)
        .generate_with_roots(WorldSeed(if rocky { 7 } else { 55 }), false)
        .unwrap();
    RunState::new(
        planet,
        GameMode::TurfRace,
        shipping.vehicle,
        shipping.job,
        &AccessibilitySettings::default(),
        false,
    )
}

fn unattended_rival_wins(resolution: u32, rocky: bool) {
    let mut run = race_run(resolution, rocky);
    let accessibility = AccessibilitySettings::default();
    let mut traveled = 0.0_f32;
    let mut airborne_ticks = 0;
    let mut previous_position = run.rival.as_ref().unwrap().state.transform.position;
    let mut previous_score = 0.0;
    let mut last_cut_tick = 0;
    let mut longest_cut_drought = 0;
    let start = std::time::Instant::now();
    for tick in 1..=240 * SIMULATION_HZ {
        run.clear_frame_events();
        run.tick(InputSnapshot::default(), &accessibility);
        let rival = &run.rival.as_ref().unwrap().state;
        traveled += rival.transform.position.distance(previous_position);
        previous_position = rival.transform.position;
        airborne_ticks += u32::from(!rival.grounded);
        let score = run.race.as_ref().unwrap();
        assert!(score.player_coverage >= 0.0 && score.rival_coverage >= previous_score);
        assert!(score.player_coverage + score.rival_coverage <= 1.0 + 1.0e-12);
        if score.rival_coverage > previous_score {
            last_cut_tick = tick;
        }
        longest_cut_drought = longest_cut_drought.max(tick - last_cut_tick);
        previous_score = score.rival_coverage;
        if tick % (30 * SIMULATION_HZ) == 0 || !run.active {
            eprintln!(
                "race rocky={rocky} mowing={resolution} t={:.1}s player={:.3}% rival={:.3}% distance={traveled:.1}m air={airborne_ticks} recoveries={} longest_drought={:.1}s outcome={:?} wall={:.2}s",
                run.simulation_seconds,
                score.player_coverage * 100.0,
                score.rival_coverage * 100.0,
                rival.recoveries,
                longest_cut_drought as f32 / SIMULATION_HZ as f32,
                score.outcome,
                start.elapsed().as_secs_f32(),
            );
        }
        if !run.active {
            break;
        }
    }
    let score = run.race.as_ref().unwrap();
    assert!(traveled > run.planet.config.base_radius * 2.0);
    assert_eq!(
        score.outcome,
        Some(RaceOutcome::RivalWon),
        "rival must actually finish a majority within 240 seconds; rocky={rocky}, mowing={resolution}, player={:.4}, rival={:.4}, distance={traveled:.1}, longest fresh-cut drought={:.1}s",
        score.player_coverage,
        score.rival_coverage,
        longest_cut_drought as f32 / SIMULATION_HZ as f32,
    );
    assert!(score.rival_coverage > 0.5);
    assert!(!run.active);
}

#[test]
fn unattended_rival_wins_flat_world_at_128() {
    unattended_rival_wins(128, false);
}

#[test]
fn unattended_rival_wins_rocky_world_at_256() {
    unattended_rival_wins(256, true);
}

#[test]
#[ignore = "manual full-resolution rival navigation and mowing endurance check"]
fn unattended_rival_wins_rocky_world_at_shipping_512() {
    unattended_rival_wins(512, true);
}

fn claim_cap(run: &mut RunState, direction: Vec3, angular_radius: f32, owner: u8) {
    let center = direction.normalize() * run.planet.config.base_radius;
    run.mowing.stamp_owned(
        MowingStamp {
            from: center,
            to: center,
            comb_direction: Vec3::Y,
            // Remove the documented conservative edge padding, so the oracle
            // hemisphere contains precisely the intended half of the sphere.
            deck_width: 2.0
                * run.planet.config.base_radius
                * (angular_radius - 1.5 / run.mowing.resolution() as f32),
            cut_delta: 1.0,
            recent_epoch: 0,
        },
        owner,
    );
}

#[derive(Debug, PartialEq)]
struct RaceObservation {
    cells: Vec<PackedMowingCell>,
    residuals: Vec<f32>,
    owners: Vec<u8>,
    player: VehicleState,
    rival: Option<VehicleState>,
    metrics: RunMetrics,
    scores: (f64, f64),
    outcome: Option<RaceOutcome>,
    finished_seconds: Option<f32>,
    bumps: u32,
    seconds: f32,
    recorded_stamps: Vec<RecordedStamp>,
}

fn observe(run: &RunState) -> RaceObservation {
    let mowing = run.mowing.snapshot();
    let race = run.race.as_ref().unwrap();
    RaceObservation {
        cells: mowing.cells,
        residuals: mowing.cut_residuals,
        owners: mowing.owners,
        player: run.vehicle.state.clone(),
        rival: run.rival.as_ref().map(|rival| rival.state.clone()),
        metrics: run.metrics.clone(),
        scores: (race.player_coverage, race.rival_coverage),
        outcome: race.outcome,
        finished_seconds: race.finished_seconds,
        bumps: race.bumps,
        seconds: run.simulation_seconds,
        recorded_stamps: run.recorder.stamps().to_vec(),
    }
}

#[test]
fn rival_majority_freezes_both_mowers_and_scores() {
    let accessibility = AccessibilitySettings::default();
    let mut run = race_run(64, false);
    claim_cap(&mut run, Vec3::X, 1.7, 2);
    let claimed = run.mowing.owned_coverage(2);
    assert!(claimed > 0.5 && claimed < 0.6);
    assert!(run.active);
    run.tick(InputSnapshot::default(), &accessibility);
    assert_eq!(
        run.race.as_ref().unwrap().outcome,
        Some(RaceOutcome::RivalWon)
    );
    assert_eq!(
        run.race.as_ref().unwrap().finished_seconds,
        Some(run.simulation_seconds)
    );
    assert!(!run.active && !run.is_victory_lap());
    assert_eq!(
        run.metrics.coverage,
        run.race.as_ref().unwrap().player_coverage
    );
    assert!(run.submit().is_none());
    let finished = observe(&run);
    for _ in 0..SIMULATION_HZ {
        run.tick(
            InputSnapshot {
                accelerate: 1.0,
                steer: 1.0,
                boost_held: true,
                recover_held: true,
                ..InputSnapshot::default()
            },
            &accessibility,
        );
    }
    assert_eq!(observe(&run), finished);
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event, RunEvent::RivalDefeated { .. }))
    );
}

#[test]
fn player_majority_removes_rival_once_and_continues_mowing_without_reset() {
    let accessibility = AccessibilitySettings::default();
    let mut run = race_run(128, false);
    let input = InputSnapshot {
        accelerate: 1.0,
        ..InputSnapshot::default()
    };
    for _ in 0..SIMULATION_HZ {
        run.clear_frame_events();
        run.tick(input, &accessibility);
    }
    let previous_player = run.vehicle.state.clone();
    let existing_cut = run.mowing.packed_cells().to_vec();
    // Leave the moving player's current neighborhood fresh, so subsequent
    // physical driving can prove that free mowing continues after the win.
    let away_from_player = -previous_player.transform.position.normalize();
    claim_cap(&mut run, away_from_player, 1.7, 1);
    assert!(run.mowing.owned_coverage(1) > 0.5);
    run.clear_frame_events();
    run.tick(input, &accessibility);
    assert!(run.active && run.is_victory_lap());
    assert_eq!(run.mode, GameMode::TurfRace);
    assert!(run.rival.is_none());
    assert!(
        run.vehicle
            .state
            .transform
            .position
            .distance(previous_player.transform.position)
            < 1.0
    );
    assert!(run.vehicle.state.speed() > 1.0);
    assert!(
        run.mowing
            .packed_cells()
            .iter()
            .zip(existing_cut)
            .all(|(now, before)| now.cut_amount() >= before.cut_amount())
    );
    let mut defeat_events = run
        .events()
        .iter()
        .filter(|event| matches!(event, RunEvent::RivalDefeated { .. }))
        .count();
    assert_eq!(defeat_events, 1);
    let finished = observe(&run);
    assert_eq!(finished.outcome, Some(RaceOutcome::PlayerWon));
    assert_eq!(finished.finished_seconds, Some(run.simulation_seconds));
    assert_eq!(run.metrics.coverage, run.mowing.coverage());
    let coverage_at_win = run.mowing.coverage();
    for _ in 0..3 * SIMULATION_HZ {
        run.clear_frame_events();
        run.tick(input, &accessibility);
        defeat_events += run
            .events()
            .iter()
            .filter(|event| matches!(event, RunEvent::RivalDefeated { .. }))
            .count();
    }
    assert_eq!(defeat_events, 1);
    assert!(run.active && run.rival.is_none());
    assert!(run.simulation_seconds > finished.seconds + 2.9);
    assert!(run.metrics.distance_traveled > finished.metrics.distance_traveled + 10.0);
    assert!(run.mowing.coverage() > coverage_at_win + 0.001);
    assert_eq!(run.metrics.coverage, run.mowing.coverage());
    assert_eq!(
        run.metrics.elapsed_seconds,
        finished.metrics.elapsed_seconds
    );
    let continued = observe(&run);
    assert_eq!(continued.scores, finished.scores);
    assert_eq!(continued.outcome, finished.outcome);
    assert_eq!(continued.finished_seconds, finished.finished_seconds);
    assert_eq!(continued.bumps, finished.bumps);
    assert_eq!(continued.owners, finished.owners);
}

#[test]
fn victory_lap_can_pause_resume_and_restart_a_fresh_rematch() {
    let accessibility = AccessibilitySettings::default();
    let mut run = race_run(64, true);
    claim_cap(&mut run, Vec3::X, 1.7, 1);
    run.tick(InputSnapshot::default(), &accessibility);
    assert!(run.is_victory_lap());
    run.clear_frame_events();
    run.paused = true;
    let paused = observe(&run);
    for _ in 0..SIMULATION_HZ {
        run.tick(InputSnapshot::default(), &accessibility);
    }
    assert_eq!(observe(&run), paused);
    assert!(run.events().is_empty());
    run.paused = false;
    run.tick(
        InputSnapshot {
            accelerate: 1.0,
            ..InputSnapshot::default()
        },
        &accessibility,
    );
    assert!(run.simulation_seconds > paused.seconds);
    assert!(run.is_victory_lap() && run.rival.is_none());
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event, RunEvent::RivalDefeated { .. }))
    );
    run.restart(&accessibility);
    assert!(!run.is_victory_lap());
    assert!(run.active && !run.paused && run.rival.is_some());
    assert_eq!(run.mode, GameMode::TurfRace);
    assert_eq!(observe(&run), observe(&race_run(64, true)));
    assert!(run.events().is_empty());
}

#[test]
fn victory_lap_milestones_and_locator_follow_global_grass_without_changing_results() {
    let accessibility = AccessibilitySettings::default();
    let mut run = race_run(128, false);
    claim_cap(&mut run, Vec3::X, 1.7, 1);
    run.tick(InputSnapshot::default(), &accessibility);
    assert!(run.is_victory_lap());
    let won = observe(&run);
    assert!(won.scores.0 > 0.5 && won.scores.0 < 0.6);
    // Fill the opposite cap during solo mowing, leaving a narrow uncut belt.
    claim_cap(&mut run, Vec3::NEG_X, 1.38, 0);
    run.clear_frame_events();
    run.tick(InputSnapshot::default(), &accessibility);
    assert!(run.mowing.coverage() > 0.95 && run.mowing.coverage() < 1.0);
    assert_eq!(run.metrics.coverage, run.mowing.coverage());
    assert!(run.events().contains(&RunEvent::CoverageMilestone(95)));
    assert!(!run.completion_available);
    assert!(run.locator_direction().is_some());
    assert!(run.active && run.submit().is_none());

    claim_cap(&mut run, Vec3::X, std::f32::consts::PI, 0);
    run.clear_frame_events();
    run.tick(InputSnapshot::default(), &accessibility);
    assert_eq!(run.metrics.coverage, 1.0);
    assert!(run.completion_available);
    assert!(run.events().contains(&RunEvent::CoverageMilestone(100)));
    assert!(run.locator_direction().is_none());
    assert!(run.active && run.is_victory_lap());
    let complete = observe(&run);
    assert_eq!(complete.scores, won.scores);
    assert_eq!(complete.finished_seconds, won.finished_seconds);
    assert_eq!(complete.outcome, won.outcome);
    assert_eq!(complete.owners, won.owners);
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event, RunEvent::RivalDefeated { .. }))
    );
}

#[test]
fn exactly_half_is_not_a_win_and_exhausted_equal_halves_draw() {
    let mut run = race_run(128, false);
    let accessibility = AccessibilitySettings::default();
    claim_cap(&mut run, Vec3::X, std::f32::consts::FRAC_PI_2, 1);
    assert_eq!(run.mowing.owned_coverage(1), 0.5);
    // Keep both decks inside already claimed turf during the arbitration tick.
    let player_spawn = run.planet.nearest_safe_point(Vec3::X);
    let rival_spawn = run
        .planet
        .nearest_safe_point(Vec3::new(1.0, 0.3, 0.0).normalize());
    run.vehicle = HoverVehicle::from_spawn(&run.planet, player_spawn, &run.vehicle_tuning);
    run.rival = Some(HoverVehicle::from_spawn(
        &run.planet,
        rival_spawn,
        &run.vehicle_tuning,
    ));
    run.tick(InputSnapshot::default(), &accessibility);
    assert!(run.active);
    assert_eq!(run.race.as_ref().unwrap().outcome, None);
    assert_eq!(run.race.as_ref().unwrap().player_coverage, 0.5);

    claim_cap(&mut run, Vec3::NEG_X, std::f32::consts::FRAC_PI_2, 2);
    assert_eq!(run.mowing.owned_coverage(2), 0.5);
    assert_eq!(run.mowing.coverage(), 1.0);
    run.tick(InputSnapshot::default(), &accessibility);
    assert_eq!(run.race.as_ref().unwrap().outcome, Some(RaceOutcome::Draw));
    assert!(!run.active);
    assert!(!run.is_victory_lap());
    assert_eq!(
        run.race.as_ref().unwrap().finished_seconds,
        Some(run.simulation_seconds)
    );
    let finished = observe(&run);
    for _ in 0..SIMULATION_HZ {
        run.tick(
            InputSnapshot {
                accelerate: 1.0,
                ..InputSnapshot::default()
            },
            &accessibility,
        );
    }
    assert_eq!(observe(&run), finished);
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event, RunEvent::RivalDefeated { .. }))
    );
}

#[test]
fn pause_restart_and_mode_switches_reset_both_mowers_and_ownership() {
    let accessibility = AccessibilitySettings::default();
    let mut run = race_run(64, true);
    for _ in 0..SIMULATION_HZ {
        run.tick(InputSnapshot::default(), &accessibility);
    }
    assert!(run.mowing.owned_coverage(2) > 0.0);
    run.paused = true;
    let paused = observe(&run);
    for _ in 0..SIMULATION_HZ {
        run.tick(InputSnapshot::default(), &accessibility);
    }
    assert_eq!(observe(&run), paused);

    run.restart(&accessibility);
    let mut fresh = race_run(64, true);
    assert_eq!(observe(&run), observe(&fresh));
    assert!(run.active && !run.paused);
    assert!(!run.tutorial_enabled);
    for _ in 0..SIMULATION_HZ {
        run.tick(InputSnapshot::default(), &accessibility);
        fresh.tick(InputSnapshot::default(), &accessibility);
    }
    assert_eq!(observe(&run), observe(&fresh));

    run.start_free_mow(&accessibility);
    assert_eq!(run.mode, GameMode::FreeMow);
    assert!(run.rival.is_none() && run.race.is_none());
    assert!(run.mowing.packed_owners().is_empty());
    assert_eq!(run.mowing.coverage(), 0.0);
    run.tick(InputSnapshot::default(), &accessibility);
    assert_eq!(run.metrics.elapsed_seconds, 0.0);
    run.start_race(&accessibility);
    assert_eq!(run.mode, GameMode::TurfRace);
    assert_eq!(observe(&run), observe(&race_run(64, true)));
}

fn chase_input(run: &RunState) -> InputSnapshot {
    let player = &run.vehicle.state;
    let up = player.transform.up;
    let toward_rival =
        run.rival.as_ref().unwrap().state.transform.position - player.transform.position;
    let travel = (toward_rival - up * toward_rival.dot(up)).normalize();
    let camera_forward = (run.camera.state.up - up * run.camera.state.up.dot(up))
        .normalize_or(player.transform.forward);
    let camera_right = camera_forward.cross(up).normalize();
    let alignment = player
        .linear_velocity
        .try_normalize()
        .map_or(1.0, |velocity| velocity.dot(travel));
    InputSnapshot {
        accelerate: travel.dot(camera_forward).max(0.0),
        brake_reverse: (-travel.dot(camera_forward)).max(0.0),
        steer: travel.dot(camera_right),
        boost_held: alignment > 0.9,
        ..InputSnapshot::default()
    }
}

fn drive_at_render_rate(fps: u32) -> RaceObservation {
    let mut run = race_run(128, false);
    let accessibility = AccessibilitySettings::default();
    // Start visibly separated; contact must result from camera-relative driving
    // and boost while the rival keeps running its normal mowing/navigation AI.
    let origin = run.planet.spawn.position.normalize();
    let ahead = run.planet.spawn.forward * (4.0 / run.planet.config.base_radius);
    let rival_spawn = run.planet.nearest_safe_point((origin + ahead).normalize());
    run.rival = Some(HoverVehicle::from_spawn(
        &run.planet,
        rival_spawn,
        &run.vehicle_tuning,
    ));
    let initial_separation = run
        .vehicle
        .state
        .transform
        .position
        .distance(run.rival.as_ref().unwrap().state.transform.position);
    assert!((3.5..4.5).contains(&initial_separation));
    let mut clock = FixedStepClock::default();
    let mut ticks = 0;
    let mut last_frame_ns = 0;
    let mut bump_events = 0;
    let mut first_bump_seconds = None;
    for frame in 1..=12 * fps {
        // Rounded cumulative timestamps avoid introducing a different count of
        // fixed ticks merely from integer-nanosecond display interval rounding.
        let frame_ns = u64::from(frame) * 1_000_000_000 / u64::from(fps);
        run.clear_frame_events();
        clock.advance(Duration::from_nanos(frame_ns - last_frame_ns), || {
            run.tick(chase_input(&run), &accessibility);
            if run.race.as_ref().unwrap().bumps > 0 {
                first_bump_seconds.get_or_insert(run.simulation_seconds);
            }
            ticks += 1;
        });
        let rival = &run.rival.as_ref().unwrap().state;
        for state in [&run.vehicle.state, rival] {
            assert!(state.transform.position.is_finite());
            assert!(state.linear_velocity.is_finite());
        }
        for event in run.events() {
            if let RunEvent::MowerBump { impulse, position } = event {
                assert!(impulse.is_finite() && *impulse > 0.0 && position.is_finite());
                assert!(
                    run.vehicle
                        .state
                        .transform
                        .position
                        .distance(rival.transform.position)
                        > 2.0
                );
                bump_events += 1;
            }
        }
        last_frame_ns = frame_ns;
    }
    assert_eq!(ticks, 12 * SIMULATION_HZ);
    let state = observe(&run);
    assert!(state.bumps > 0);
    assert_eq!(state.bumps, bump_events);
    assert!(first_bump_seconds.unwrap() > 0.1);
    assert!(state.player.grounded && state.rival.as_ref().unwrap().grounded);
    assert!(state.scores.0 > 0.0 && state.scores.1 > 0.0);
    eprintln!(
        "bumper gameplay fps={fps}: start={initial_separation:.2}m, first impact={:.2}s, {} recorded bumps, {bump_events} events, both grounded",
        first_bump_seconds.unwrap(),
        state.bumps
    );
    state
}

#[test]
fn render_rate_does_not_change_two_mower_physics_bumps_or_claims() {
    let expected = drive_at_render_rate(120);
    for fps in [30, 60, 240] {
        assert_eq!(drive_at_render_rate(fps), expected, "render rate {fps}");
    }
}
