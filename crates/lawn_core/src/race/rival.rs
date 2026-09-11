//! Bounded surface rollouts for mowing, with a coarse graph for relocation.
//!
//! The driver predicts only tangent motion and sampled deck coverage. It never
//! clones physics or the mowing field, and keeps all decision scratch reusable.

use std::{cmp::Ordering, collections::BinaryHeap};

use glam::Vec3;

use super::reward::{claim_reward, expected_cut_for_pass};

use crate::{
    FIXED_DT, SIMULATION_HZ,
    config::VehicleTuning,
    cube_map::{CubeCell, CubeFace, cell_center_direction, direction_to_cell, offset_cell},
    input::InputSnapshot,
    mowing::{CUT_COVERAGE_THRESHOLD, GRASS_PATCH_RESOLUTION, MowingField},
    planet::{Planet, SurfaceMaterial},
    vehicle::VehicleState,
};

const NAV_RESOLUTION: u32 = GRASS_PATCH_RESOLUTION;
const DECISION_TICKS: u32 = SIMULATION_HZ / 10;
const PLAN_SECONDS: f32 = 1.0;
const ROLLOUT_STEPS: usize = 15;
const ROLLOUT_DT: f32 = 0.1;
const DECK_SAMPLES: usize = 5;
const ANGLES: [f32; 15] = [
    0.0,
    0.14,
    -0.14,
    0.3,
    -0.3,
    0.55,
    -0.55,
    0.9,
    -0.9,
    1.35,
    -1.35,
    1.9,
    -1.9,
    2.5,
    std::f32::consts::PI,
];

#[derive(Debug)]
struct NavNode {
    direction: Vec3,
    position: Vec3,
    neighbors: Vec<(usize, f32)>,
    walkable: bool,
}

#[derive(Debug)]
pub(crate) struct RivalAi {
    nodes: Vec<NavNode>,
    path: Vec<usize>,
    next: usize,
    goal_direction: Vec3,
    cleanup: bool,
    replan_seconds: f32,
    decision_ticks: u32,
    travel: Vec3,
    throttle: f32,
    boost: bool,
    recoveries: u32,
    distances: Vec<f32>,
    previous: Vec<usize>,
    frontier: BinaryHeap<Visit>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Visit {
    node: usize,
    cost: f32,
}
impl Eq for Visit {}
impl Ord for Visit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.node.cmp(&self.node))
    }
}
impl PartialOrd for Visit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug)]
struct Route {
    direction: Vec3,
    throttle: f32,
    boost: bool,
    gain: f32,
    score: f32,
}

fn index(cell: CubeCell) -> usize {
    cell.face.index() * (NAV_RESOLUTION * NAV_RESOLUTION) as usize
        + (cell.y * NAV_RESOLUTION + cell.x) as usize
}

fn tangent(vector: Vec3, up: Vec3, fallback: Vec3) -> Vec3 {
    (vector - up * vector.dot(up)).normalize_or(fallback)
}

impl RivalAi {
    pub(super) fn new(planet: &Planet) -> Self {
        let mut nodes = Vec::new();
        for face in CubeFace::ALL {
            for y in 0..NAV_RESOLUTION {
                for x in 0..NAV_RESOLUTION {
                    let direction = cell_center_direction(CubeCell { face, x, y }, NAV_RESOLUTION);
                    let terrain = planet.terrain_cell(direction);
                    nodes.push(NavNode {
                        direction,
                        position: planet.surface_point(direction),
                        neighbors: Vec::new(),
                        walkable: planet.is_mowable(direction) && terrain.slope_radians < 0.85,
                    });
                }
            }
        }
        for face in CubeFace::ALL {
            for y in 0..NAV_RESOLUTION {
                for x in 0..NAV_RESOLUTION {
                    let cell = CubeCell { face, x, y };
                    let from = index(cell);
                    if !nodes[from].walkable {
                        continue;
                    }
                    for (dx, dy) in [
                        (1, 0),
                        (-1, 0),
                        (0, 1),
                        (0, -1),
                        (1, 1),
                        (1, -1),
                        (-1, 1),
                        (-1, -1),
                    ] {
                        let to = index(offset_cell(cell, dx, dy, NAV_RESOLUTION));
                        let midpoint = (nodes[from].direction + nodes[to].direction).normalize();
                        if nodes[to].walkable && planet.is_mowable(midpoint) {
                            let distance = nodes[from].position.distance(nodes[to].position);
                            nodes[from].neighbors.push((to, distance));
                        }
                    }
                }
            }
        }
        let count = nodes.len();
        Self {
            nodes,
            path: Vec::new(),
            next: 0,
            goal_direction: Vec3::ZERO,
            cleanup: false,
            replan_seconds: 0.0,
            decision_ticks: 0,
            travel: Vec3::ZERO,
            throttle: 1.0,
            boost: false,
            recoveries: 0,
            distances: vec![f32::INFINITY; count],
            previous: vec![usize::MAX; count],
            frontier: BinaryHeap::new(),
        }
    }

    fn plan(&mut self, mowing: &MowingField, rival: &VehicleState, tuning: &VehicleTuning) {
        self.cleanup = mowing.coverage() > 0.9;
        let here = rival.transform.position.normalize();
        let start = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.walkable)
            .max_by(|(_, a), (_, b)| a.direction.dot(here).total_cmp(&b.direction.dot(here)))
            .map_or(0, |(i, _)| i);
        self.distances.fill(f32::INFINITY);
        self.previous.fill(usize::MAX);
        self.frontier.clear();
        self.distances[start] = 0.0;
        self.frontier.push(Visit {
            node: start,
            cost: 0.0,
        });
        while let Some(Visit { node, cost }) = self.frontier.pop() {
            if cost > self.distances[node] {
                continue;
            }
            for &(neighbor, edge) in &self.nodes[node].neighbors {
                let next_cost = cost + edge;
                if next_cost < self.distances[neighbor] {
                    self.distances[neighbor] = next_cost;
                    self.previous[neighbor] = node;
                    self.frontier.push(Visit {
                        node: neighbor,
                        cost: next_cost,
                    });
                }
            }
        }
        let patches = mowing.remaining_grass_patches();
        let mut best = start;
        let mut best_score = 0.0;
        for (i, node) in self.nodes.iter().enumerate() {
            let graph_distance = self.distances[i];
            let remaining = patches[i].direction();
            if !graph_distance.is_finite()
                || (!self.cleanup && graph_distance < tuning.mower_width)
                || (self.cleanup && remaining.is_none())
            {
                continue;
            }
            let target = remaining.unwrap_or(node.direction);
            // The current patch can contain the last available grass. Include
            // its real centroid and do not relocate to an empty patch merely
            // because its neighbors still have grass.
            let distance = if !self.cleanup {
                graph_distance
            } else if i == start {
                (target - here).length() * node.position.length()
            } else {
                graph_distance + (target - node.direction).length() * node.position.length()
            };
            // A patch's real remaining area makes a broad meadow preferable to
            // a tiny uncut edge. Neighbors provide a cheap estimate of runway.
            let area = patches[i].area as f32
                + node
                    .neighbors
                    .iter()
                    .map(|&(j, _)| patches[j].area as f32 * 0.5)
                    .sum::<f32>();
            let direction = tangent(
                if self.cleanup { target } else { node.direction },
                here,
                rival.transform.forward,
            );
            let alignment = tangent(rival.linear_velocity, here, self.travel).dot(direction);
            let score = area / (distance + tuning.max_speed * 0.6) * (1.0 + 0.15 * alignment);
            if score > best_score {
                best_score = score;
                best = i;
            }
        }
        self.path.clear();
        let mut cursor = best;
        while cursor != start && cursor != usize::MAX {
            self.path.push(cursor);
            cursor = self.previous[cursor];
        }
        self.path.reverse();
        if best == start && best_score > 0.0 {
            self.path.push(start);
        }
        self.goal_direction = patches[best]
            .direction()
            .unwrap_or(self.nodes[best].direction);
        self.next = 0;
        self.replan_seconds = PLAN_SECONDS;
    }

    fn guide(
        &mut self,
        planet: &Planet,
        rival: &VehicleState,
        tuning: &VehicleTuning,
    ) -> Option<Vec3> {
        let here = rival.transform.position.normalize();
        // Include the final waypoint: a completed strip must never pull the
        // mower backward until a timer expires.
        while let Some(&i) = self.path.get(self.next) {
            let final_waypoint = self.next + 1 == self.path.len();
            let target = if final_waypoint {
                self.goal_direction
            } else {
                self.nodes[i].direction
            };
            let arrival = if final_waypoint && self.cleanup {
                (tuning.mower_width * 0.18).min(0.4)
            } else {
                tuning.mower_width * 0.65
            };
            if (target - here).length() * planet.config.base_radius > arrival {
                break;
            }
            self.next += 1;
        }
        let &first = self.path.get(self.next)?;
        let mut target = first;
        let lookahead = (rival.speed() * 0.35).max(tuning.mower_width);
        for &i in self.path.iter().skip(self.next + 1) {
            let delta = self.nodes[i].direction - here;
            if delta.length() * planet.config.base_radius > lookahead {
                break;
            }
            let midpoint = (here + self.nodes[i].direction).normalize();
            if !planet.is_mowable(midpoint) {
                break;
            }
            target = i;
        }
        let target_direction = if Some(&target) == self.path.last() {
            self.goal_direction
        } else {
            self.nodes[target].direction
        };
        Some(tangent(target_direction, here, rival.transform.forward))
    }

    fn decide(
        &mut self,
        planet: &Planet,
        mowing: &MowingField,
        rival: &VehicleState,
        player: &VehicleState,
        tuning: &VehicleTuning,
        boost_allowed: bool,
    ) {
        let here = rival.transform.position.normalize();
        let forward = tangent(rival.transform.forward, here, Vec3::X);
        let momentum = tangent(rival.linear_velocity, here, forward);
        let right = momentum.cross(here).normalize();
        let mut directions = [Vec3::ZERO; ANGLES.len() + 2];
        for (direction, angle) in directions.iter_mut().zip(ANGLES) {
            let (sin, cos) = angle.sin_cos();
            *direction = momentum * cos + right * sin;
        }
        directions[ANGLES.len()] = tangent(self.travel, here, momentum);
        directions[ANGLES.len() + 1] = momentum;
        let mut guide = self.guide(planet, rival, tuning);
        // Search locally first; graph searches are only needed when productive
        // nearby strips run out. A cached path remains useful between decisions.
        if let Some(direction) = guide {
            directions[ANGLES.len() + 1] = direction;
        }
        let mut best = Route {
            direction: momentum,
            throttle: 1.0,
            boost: false,
            gain: 0.0,
            score: f32::NEG_INFINITY,
        };
        for direction in directions {
            for boost in [false, true] {
                // Repeated tiny bursts reset the recharge delay and waste most
                // of the boost cycle. Finish a burst, then refill before starting.
                if boost && (!boost_allowed || !self.boost_ready(rival, tuning)) {
                    continue;
                }
                let route = self.rollout(
                    planet, mowing, rival, player, tuning, direction, boost, None, false,
                );
                if route.score > best.score {
                    best = route;
                }
            }
        }
        let productive_gain =
            tuning.mower_width * tuning.max_speed * ROLLOUT_DT * ROLLOUT_STEPS as f32 * 0.45;
        if best.gain < productive_gain {
            if self.replan_seconds <= 0.0 {
                self.plan(mowing, rival, tuning);
                guide = self.guide(planet, rival, tuning);
            }
            if let Some(goal) = guide {
                directions[ANGLES.len() + 1] = goal;
                for direction in directions {
                    // Reuse a small, deterministic candidate set for relocation.
                    let route = self.rollout(
                        planet,
                        mowing,
                        rival,
                        player,
                        tuning,
                        direction,
                        false,
                        Some(goal),
                        false,
                    );
                    if route.score > best.score {
                        best = route;
                    }
                }
                // A final island of grass needs a controlled approach rather
                // than a full-speed pass that the obstacle check rejects.
                // One extra rollout follows the target and brakes as it nears.
                if self.cleanup
                    && self.next + 1 == self.path.len()
                    && (self.goal_direction - here).length() * planet.config.base_radius
                        < tuning.max_speed
                {
                    let route = self.rollout(
                        planet,
                        mowing,
                        rival,
                        player,
                        tuning,
                        goal,
                        false,
                        Some(goal),
                        true,
                    );
                    if route.score > best.score {
                        best = route;
                    }
                }
            }
        } else {
            self.path.clear();
            self.next = 0;
        }
        self.travel = best.direction;
        self.throttle = best.throttle;
        self.boost = best.boost;
        self.decision_ticks = DECISION_TICKS;
    }

    fn boost_ready(&self, rival: &VehicleState, tuning: &VehicleTuning) -> bool {
        rival.boost_charge > 0.0
            && (self.boost || rival.boost_charge >= tuning.boost_capacity_seconds * 0.8)
    }

    fn rollout(
        &self,
        planet: &Planet,
        mowing: &MowingField,
        rival: &VehicleState,
        player: &VehicleState,
        tuning: &VehicleTuning,
        direction: Vec3,
        boost: bool,
        guide: Option<Vec3>,
        approach: bool,
    ) -> Route {
        let mut here = rival.transform.position.normalize();
        let mut heading = direction;
        let mut velocity = rival.linear_velocity - here * rival.linear_velocity.dot(here);
        let mut gain = 0.0;
        let mut score = 0.0;
        let mut swept = [Vec3::ZERO; ROLLOUT_STEPS + 1];
        swept[0] = here;
        let mut seen = [usize::MAX; ROLLOUT_STEPS * DECK_SAMPLES];
        let mut seen_count = 0;
        let radius = planet.config.base_radius;
        let half_width = tuning.mower_width * 0.5;
        let effective_radius = half_width + 1.5 * radius / mowing.resolution() as f32;
        let boost_seconds = rival.boost_charge;
        let target = (guide.is_some() && self.cleanup && self.next + 1 == self.path.len())
            .then_some(self.goal_direction);
        let throttle = if approach {
            ((self.goal_direction - here).length() * radius * 2.0 / tuning.max_speed).min(1.0)
        } else {
            1.0
        };
        let player_here = player.transform.position.normalize();
        let player_velocity =
            player.linear_velocity - player_here * player.linear_velocity.dot(player_here);
        for step in 0..ROLLOUT_STEPS {
            let time = step as f32 * ROLLOUT_DT;
            let boosting = boost && time < boost_seconds;
            let mut speed = if boosting {
                tuning.boost_max_speed
            } else {
                tuning.max_speed
            };
            if approach {
                heading = tangent(self.goal_direction, here, heading);
                speed = speed.min((self.goal_direction - here).length() * radius * 2.0);
            }
            let alignment = velocity.normalize_or(heading).dot(heading);
            let response_time = if alignment < 0.8 {
                tuning.direction_change_time_90_percent
            } else if velocity.length() > speed + 0.01 {
                tuning.braking_time_90_percent
            } else {
                tuning.acceleration_time_90_percent
            };
            let response = std::f32::consts::LN_10 / response_time.max(0.01)
                * if boosting {
                    tuning.boost_acceleration_multiplier
                } else {
                    1.0
                };
            velocity = velocity.lerp(heading * speed, 1.0 - (-response * ROLLOUT_DT).exp());
            let next = (here + velocity * (ROLLOUT_DT / radius)).normalize();
            let middle = (here + next).normalize();
            let terrain = planet.terrain_cell(middle);
            let travel = tangent(velocity, middle, heading);
            let across = travel.cross(middle).normalize();
            // Check a body-width corridor, not just a destination point. Small
            // edge overlaps lose grass reward but do not prohibit mowing near rock.
            let clear = [-0.65_f32, 0.0, 0.65].into_iter().all(|offset| {
                let sample = (middle + across * (half_width * offset / radius)).normalize();
                let cell = planet.terrain_cell(sample);
                cell.material == SurfaceMaterial::Grass && cell.slope_radians < 0.85
            });
            if !clear {
                // An imminent obstacle is worse than one near the far horizon.
                score -= 18.0 * (1.0 - step as f32 / ROLLOUT_STEPS as f32);
                break;
            }
            let length = (next - here).length() * terrain.radius
                / terrain.normal.dot(middle).clamp(0.5, 1.0);
            let sample_area = length * tuning.mower_width / DECK_SAMPLES as f32;
            let discount = 1.0 - 0.25 * step as f32 / ROLLOUT_STEPS as f32;
            for across_index in 0..DECK_SAMPLES {
                let offset =
                    ((across_index as f32 + 0.5) / DECK_SAMPLES as f32 * 2.0 - 1.0) * half_width;
                let sample = (middle + across * (offset / radius)).normalize();
                let cell = direction_to_cell(sample, mowing.resolution());
                if !mowing.is_mowable(cell)
                    || mowing.owner(cell) != 0
                    || mowing.cell(cell).cut_amount() >= CUT_COVERAGE_THRESHOLD
                {
                    continue;
                }
                let id = cell.face.index() * (mowing.resolution() * mowing.resolution()) as usize
                    + (cell.y * mowing.resolution() + cell.x) as usize;
                if seen[..seen_count].contains(&id) {
                    continue;
                }
                seen[seen_count] = id;
                seen_count += 1;
                // Reverse maneuvers can overlap their own projected trail even
                // when the quadrature samples address different fine-grid cells.
                if (0..step.saturating_sub(1)).any(|i| {
                    let a = swept[i];
                    let segment = swept[i + 1] - a;
                    let t = ((sample - a).dot(segment) / segment.length_squared().max(1.0e-10))
                        .clamp(0.0, 1.0);
                    sample.distance_squared(a + segment * t) * radius * radius
                        < half_width * half_width
                }) {
                    continue;
                }
                // Only discount a player's likely immediate swept strip. A
                // player near a meadow is not by itself a reason to chase it.
                let delta = (sample - player_here) * radius;
                let player_time =
                    delta.dot(player_velocity) / player_velocity.length_squared().max(1.0);
                let player_miss = (delta - player_velocity * player_time).length();
                let contested =
                    if player_time >= 0.0 && player_time < time && player_miss < half_width {
                        0.15
                    } else {
                        1.0
                    };
                let expected_cut =
                    expected_cut_for_pass(tuning, velocity.length(), offset, effective_radius);
                let reward = claim_reward(
                    mowing.cell(cell).cut_amount(),
                    mowing.owner(cell),
                    expected_cut,
                );
                gain += sample_area * contested * reward;
                score += sample_area * discount * contested * reward;
            }
            if let Some(goal) = guide {
                let progress = target.map_or(length * direction.dot(goal), |destination| {
                    ((destination - here).length() - (destination - next).length()) * radius
                });
                score += progress * 0.7 * discount;
            }
            swept[step + 1] = next;
            heading = tangent(heading, next, travel);
            velocity -= next * velocity.dot(next);
            here = next;
        }
        // Keep productive passes stable when neighboring sample patterns tie.
        let momentum = rival.linear_velocity.normalize_or(direction);
        score += 0.35 * direction.dot(momentum);
        score += 0.15 * direction.dot(self.travel);
        Route {
            direction,
            throttle,
            boost,
            gain,
            score,
        }
    }

    pub(crate) fn drive(
        &mut self,
        planet: &Planet,
        mowing: &MowingField,
        rival: &VehicleState,
        player: &VehicleState,
        tuning: &VehicleTuning,
        boost_allowed: bool,
    ) -> InputSnapshot {
        self.replan_seconds -= FIXED_DT;
        self.decision_ticks = self.decision_ticks.saturating_sub(1);
        if self.recoveries != rival.recoveries {
            self.recoveries = rival.recoveries;
            self.decision_ticks = 0;
            self.replan_seconds = 0.0;
            self.path.clear();
        }
        if self.decision_ticks == 0 {
            self.decide(planet, mowing, rival, player, tuning, boost_allowed);
        }
        let up = rival.transform.up;
        self.travel = tangent(self.travel, up, rival.transform.forward);
        let forward = rival.transform.forward;
        let right = forward.cross(up).normalize();
        InputSnapshot {
            accelerate: self.travel.dot(forward).max(0.0) * self.throttle,
            brake_reverse: (-self.travel.dot(forward)).max(0.0) * self.throttle,
            steer: self.travel.dot(right) * self.throttle,
            boost_held: self.boost && rival.grounded && boost_allowed,
            recover_held: rival.stuck_seconds > 1.5,
            ..InputSnapshot::default()
        }
    }
}

#[cfg(test)]
#[path = "rival_tests.rs"]
mod tests;
