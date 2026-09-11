use super::*;
use crate::{GeneratorConfig, PlanetGenerator, WorldSeed, mowing::MowingStamp};

fn arena() -> (
    Planet,
    MowingField,
    VehicleTuning,
    VehicleState,
    VehicleState,
) {
    let planet = PlanetGenerator::new(
        crate::planet::CURRENT_GENERATOR_VERSION,
        GeneratorConfig {
            mountain_count_min: 0,
            mountain_count_max: 0,
            mowable_ratio_min: 1.0,
            mowable_ratio_max: 1.0,
            rolling_amplitude: 0.0,
            ..GeneratorConfig::test_quality()
        },
    )
    .generate_with_roots(WorldSeed(55), false)
    .unwrap();
    let mut mowing = MowingField::from_planet(&planet);
    mowing.enable_ownership();
    let tuning = VehicleTuning::default();
    let rival = VehicleState::at_spawn(planet.spawn, &tuning);
    let mut player = rival.clone();
    player.transform.position = -rival.transform.position;
    (planet, mowing, tuning, rival, player)
}

#[test]
fn completed_final_waypoint_does_not_pull_the_mower_backward() {
    let (planet, _, tuning, rival, _) = arena();
    let mut ai = RivalAi::new(&planet);
    ai.path.push(index(direction_to_cell(
        rival.transform.position,
        NAV_RESOLUTION,
    )));
    ai.goal_direction = rival.transform.position.normalize();
    assert!(ai.guide(&planet, &rival, &tuning).is_none());
    assert_eq!(ai.next, ai.path.len());
}

#[test]
fn boost_finishes_a_burst_then_waits_for_a_useful_refill() {
    let (planet, _, tuning, mut rival, _) = arena();
    let mut ai = RivalAi::new(&planet);
    rival.boost_charge = 0.2;
    assert!(!ai.boost_ready(&rival, &tuning));
    ai.boost = true;
    assert!(ai.boost_ready(&rival, &tuning));
    rival.boost_charge = 0.0;
    assert!(!ai.boost_ready(&rival, &tuning));
    ai.boost = false;
    rival.boost_charge = tuning.boost_capacity_seconds;
    assert!(ai.boost_ready(&rival, &tuning));
}

#[test]
fn owned_and_covered_ground_never_earns_predicted_area() {
    let (planet, mut mowing, tuning, mut rival, player) = arena();
    rival.linear_velocity = rival.transform.forward * tuning.max_speed;
    let ai = RivalAi::new(&planet);
    let forward = rival.transform.forward;
    let fresh = ai.rollout(
        &planet, &mowing, &rival, &player, &tuning, forward, false, None, false,
    );
    let reverse = ai.rollout(
        &planet, &mowing, &rival, &player, &tuning, -forward, false, None, false,
    );
    assert!(fresh.gain > 0.0);
    assert!(
        fresh.gain > reverse.gain,
        "turning back over the projected trail must not inflate reward"
    );
    mowing.stamp_owned(
        MowingStamp {
            from: rival.transform.position,
            to: rival.transform.position,
            comb_direction: forward,
            deck_width: std::f32::consts::TAU * planet.config.base_radius,
            cut_delta: 1.0,
            recent_epoch: 0,
        },
        1,
    );
    let covered = ai.rollout(
        &planet, &mowing, &rival, &player, &tuning, forward, true, None, false,
    );
    assert_eq!(covered.gain, 0.0);
}

#[test]
fn decisions_have_a_fixed_cadence_and_recovery_invalidates_the_route() {
    let (planet, mowing, tuning, mut rival, player) = arena();
    let mut ai = RivalAi::new(&planet);
    let input = ai.drive(&planet, &mowing, &rival, &player, &tuning, false);
    assert!(!input.boost_held);
    assert!((input.accelerate - input.brake_reverse).hypot(input.steer) > 0.99);
    for _ in 1..DECISION_TICKS {
        ai.drive(&planet, &mowing, &rival, &player, &tuning, false);
    }
    assert_eq!(ai.decision_ticks, 1);
    ai.drive(&planet, &mowing, &rival, &player, &tuning, false);
    assert_eq!(ai.decision_ticks, DECISION_TICKS);
    ai.decision_ticks = 5;
    ai.path.push(0);
    rival.recoveries += 1;
    let input = ai.drive(&planet, &mowing, &rival, &player, &tuning, false);
    assert_eq!(ai.decision_ticks, DECISION_TICKS);
    assert!(ai.path.is_empty());
    assert!(input.accelerate.is_finite() && input.steer.is_finite());
}

#[test]
fn sparse_endgame_approaches_and_claims_a_nearby_last_patch() {
    let (planet, mut mowing, tuning, rival, player) = arena();
    let here = rival.transform.position.normalize();
    let target = direction_to_cell(
        here + rival.transform.forward * (2.0 / planet.config.base_radius),
        mowing.resolution(),
    );
    let flat_index = target.face.index() * (mowing.resolution() * mowing.resolution()) as usize
        + (target.y * mowing.resolution() + target.x) as usize;
    let mut snapshot = mowing.snapshot();
    snapshot.cells.fill(crate::mowing::PackedMowingCell(255));
    snapshot.owners.fill(1);
    snapshot.cells[flat_index] = crate::mowing::PackedMowingCell::default();
    snapshot.owners[flat_index] = 0;
    mowing.restore(&snapshot).unwrap();
    let mut vehicle = crate::vehicle::HoverVehicle::new(&planet, &tuning);
    let mut ai = RivalAi::new(&planet);
    let input = ai.drive(&planet, &mowing, &vehicle.state, &player, &tuning, false);
    let movement = (input.accelerate - input.brake_reverse).hypot(input.steer);
    assert!(
        movement > 0.0 && movement < 1.0,
        "approach should brake before overshooting"
    );
    for _ in 0..3 * SIMULATION_HZ {
        let input = ai.drive(&planet, &mowing, &vehicle.state, &player, &tuning, false);
        let tick = vehicle.tick(
            &planet,
            &tuning,
            input,
            vehicle.state.transform.forward,
            false,
            FIXED_DT,
        );
        if vehicle.state.grounded && !tick.recovered {
            mowing.stamp_owned(
                MowingStamp {
                    from: tick.deck_from,
                    to: tick.deck_to,
                    comb_direction: vehicle.state.transform.forward,
                    deck_width: tuning.mower_width,
                    cut_delta: tuning.cut_rate_per_second
                        * FIXED_DT.max(tick.traveled_distance / tuning.max_speed),
                    recent_epoch: 0,
                },
                2,
            );
        }
        if mowing.owner(target) == 2 {
            break;
        }
    }
    assert_eq!(
        mowing.owner(target),
        2,
        "nearby remaining grass must actually be claimed"
    );
    assert_eq!(vehicle.state.recoveries, 0);
}

#[test]
#[ignore = "manual isolated AI decision cost; use --release --nocapture"]
fn benchmark_rival_decision_cost() {
    let (planet, mut mowing, tuning, mut rival, player) = arena();
    let mut ai = RivalAi::new(&planet);
    let _ = mowing.remaining_grass_patches();
    rival.linear_velocity = rival.transform.forward * tuning.max_speed;
    for covered in [false, true] {
        if covered {
            mowing.stamp_owned(
                MowingStamp {
                    from: rival.transform.position,
                    to: rival.transform.position,
                    comb_direction: rival.transform.forward,
                    deck_width: std::f32::consts::TAU * planet.config.base_radius,
                    cut_delta: 1.0,
                    recent_epoch: 0,
                },
                1,
            );
        }
        let mut times = Vec::with_capacity(300);
        for iteration in 0..300 {
            ai.replan_seconds = if iteration % 10 == 0 { 0.0 } else { 1.0 };
            let start = std::time::Instant::now();
            ai.decide(&planet, &mowing, &rival, &player, &tuning, true);
            times.push(start.elapsed());
        }
        times.sort_unstable();
        eprintln!(
            "isolated AI covered={covered}: mean={:?}, p95={:?}, max={:?}",
            times.iter().sum::<std::time::Duration>() / times.len() as u32,
            times[times.len() * 95 / 100],
            times.last().unwrap()
        );
    }
}
