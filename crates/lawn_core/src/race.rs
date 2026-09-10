//! Deterministic turf-race objectives and a surface-aware rival driver.

use std::{cmp::Ordering, collections::BinaryHeap};

use glam::Vec3;

use crate::{
    FIXED_DT,
    cube_map::{
        CubeCell, CubeFace, cell_center_direction, direction_to_cell, offset_cell, tangent_frame,
    },
    input::InputSnapshot,
    mowing::MowingField,
    planet::{Planet, SpawnPoint},
    vehicle::VehicleState,
};

const NAV_RESOLUTION: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaceOutcome {
    PlayerWon,
    RivalWon,
    Draw,
}

#[derive(Debug)]
pub struct RaceState {
    pub player_coverage: f64,
    pub rival_coverage: f64,
    pub outcome: Option<RaceOutcome>,
    /// Active simulation time when the race ended, retained during a victory lap.
    pub finished_seconds: Option<f32>,
    pub bumps: u32,
    pub(crate) ai: RivalAi,
    pub(crate) bump_cooldown: f32,
}

impl RaceState {
    #[must_use]
    pub fn new(planet: &Planet) -> Self {
        Self {
            player_coverage: 0.0,
            rival_coverage: 0.0,
            outcome: None,
            finished_seconds: None,
            bumps: 0,
            ai: RivalAi::new(planet),
            bump_cooldown: 0.0,
        }
    }

    pub(crate) fn update_score(&mut self, mowing: &MowingField, elapsed_seconds: f32) {
        if self.outcome.is_some() {
            return;
        }
        self.player_coverage = mowing.owned_coverage(1);
        self.rival_coverage = mowing.owned_coverage(2);
        self.outcome = if self.player_coverage > 0.5 {
            Some(RaceOutcome::PlayerWon)
        } else if self.rival_coverage > 0.5 {
            Some(RaceOutcome::RivalWon)
        } else if self.player_coverage + self.rival_coverage >= 1.0 - 1.0e-8 {
            Some(RaceOutcome::Draw)
        } else {
            None
        };
        if self.outcome.is_some() {
            self.finished_seconds = Some(elapsed_seconds);
        }
    }
}

/// Starts in the same hemisphere, with room to accelerate and meet over fresh grass.
#[must_use]
pub fn rival_spawn(planet: &Planet) -> SpawnPoint {
    let origin = planet.spawn.position.normalize();
    let right = planet.spawn.forward.cross(planet.spawn.up).normalize();
    let mut best = planet.spawn;
    let mut best_distance = 0.0_f32;
    for direction in [right, -right, planet.spawn.forward, -planet.spawn.forward] {
        let candidate = planet.nearest_safe_point((origin + direction * 0.52).normalize());
        let distance = candidate.position.distance(planet.spawn.position);
        if distance > best_distance && distance < planet.config.base_radius * 1.1 {
            best = candidate;
            best_distance = distance;
        }
    }
    best
}

#[derive(Debug)]
struct NavNode {
    direction: Vec3,
    neighbors: Vec<(usize, f32)>,
    walkable: bool,
}

#[derive(Debug)]
pub(crate) struct RivalAi {
    nodes: Vec<NavNode>,
    path: Vec<usize>,
    next: usize,
    replan_seconds: f32,
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

fn index(cell: CubeCell) -> usize {
    (cell.face.index() * (NAV_RESOLUTION * NAV_RESOLUTION) as usize)
        + (cell.y * NAV_RESOLUTION + cell.x) as usize
}

impl RivalAi {
    fn new(planet: &Planet) -> Self {
        let mut nodes = Vec::new();
        for face in CubeFace::ALL {
            for y in 0..NAV_RESOLUTION {
                for x in 0..NAV_RESOLUTION {
                    let direction = cell_center_direction(CubeCell { face, x, y }, NAV_RESOLUTION);
                    let terrain = planet.terrain_cell(direction);
                    nodes.push(NavNode {
                        direction,
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
                    for neighbor in [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .map(|(dx, dy)| offset_cell(cell, dx, dy, NAV_RESOLUTION))
                    {
                        let to = index(neighbor);
                        let midpoint = (nodes[from].direction + nodes[to].direction).normalize();
                        if nodes[to].walkable && planet.is_mowable(midpoint) {
                            let distance = planet
                                .surface_point(nodes[from].direction)
                                .distance(planet.surface_point(nodes[to].direction));
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
            replan_seconds: 0.0,
            distances: vec![f32::INFINITY; count],
            previous: vec![usize::MAX; count],
            frontier: BinaryHeap::new(),
        }
    }

    fn freshness(direction: Vec3, planet: &Planet, mowing: &MowingField) -> f32 {
        let (right, forward) = tangent_frame(direction);
        let width = 0.85 / planet.config.base_radius;
        let mut fresh = 0.0;
        for offset in [
            Vec3::ZERO,
            right * width,
            -right * width,
            forward * width,
            -forward * width,
        ] {
            let sample = (direction + offset).normalize();
            if planet.is_mowable(sample) {
                let cell = direction_to_cell(sample, mowing.resolution());
                fresh += 1.0 - f32::from(mowing.cell(cell).cut_amount()) / 255.0;
            }
        }
        fresh / 5.0
    }

    fn plan(
        &mut self,
        planet: &Planet,
        mowing: &MowingField,
        rival: &VehicleState,
        player: &VehicleState,
    ) {
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
        let mut best = start;
        let mut best_score = 0.0_f32;
        for (i, node) in self.nodes.iter().enumerate() {
            let distance = self.distances[i];
            if !distance.is_finite() || distance < 2.0 {
                continue;
            }
            let fresh = Self::freshness(node.direction, planet, mowing);
            let player_distance = planet
                .surface_point(node.direction)
                .distance(player.transform.position);
            // Prefer productive nearby strips, with a mild preference for
            // meadows the player can also reach. Never chase through cut turf
            // just to ram: the opponent's objective remains winning the lawn.
            let score = fresh / (distance + 4.0) * (1.0 + 2.0 / (player_distance + 3.0));
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
        self.next = 0;
        self.replan_seconds = 0.65;
    }

    pub(crate) fn drive(
        &mut self,
        planet: &Planet,
        mowing: &MowingField,
        rival: &VehicleState,
        player: &VehicleState,
    ) -> InputSnapshot {
        self.replan_seconds -= FIXED_DT;
        if self.replan_seconds <= 0.0 || self.next >= self.path.len() {
            self.plan(planet, mowing, rival, player);
        }
        let position = rival.transform.position;
        while self.next + 1 < self.path.len()
            && position.distance(planet.surface_point(self.nodes[self.path[self.next]].direction))
                < 1.8
        {
            self.next += 1;
        }
        let Some(&target) = self.path.get(self.next) else {
            return InputSnapshot {
                accelerate: 0.6,
                steer: 0.35,
                ..InputSnapshot::default()
            };
        };
        let up = rival.transform.up;
        let target_direction = self.nodes[target].direction;
        let to_target = target_direction - position.normalize();
        let travel = (to_target - up * to_target.dot(up)).normalize_or(rival.transform.forward);
        let forward = rival.transform.forward;
        let right = forward.cross(up).normalize();
        let alignment = rival
            .linear_velocity
            .try_normalize()
            .map_or(1.0, |v| v.dot(travel));
        let throttle = if alignment < 0.6 { 0.72 } else { 0.92 };
        let clear_ahead =
            (position.normalize() + travel * (5.0 / planet.config.base_radius)).normalize();
        let boost = alignment > 0.94
            && self.path.len().saturating_sub(self.next) >= 4
            && planet.is_mowable(clear_ahead)
            && Self::freshness(clear_ahead, planet, mowing) > 0.6
            && rival.boost_charge > 0.35;
        InputSnapshot {
            accelerate: travel.dot(forward).max(0.0) * throttle,
            brake_reverse: (-travel.dot(forward)).max(0.0) * throttle,
            steer: travel.dot(right) * throttle,
            boost_held: boost,
            recover_held: rival.stuck_seconds > 1.5,
            ..InputSnapshot::default()
        }
    }
}
