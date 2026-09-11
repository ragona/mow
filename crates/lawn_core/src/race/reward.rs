//! Cheap ownership rewards for a predicted pass through a mowing disc.

use crate::{config::VehicleTuning, mowing::CUT_COVERAGE_THRESHOLD};

/// Estimate fractional cutting work at a lateral offset during one full pass.
///
/// `effective_radius` includes the mowing field's conservative 1.5-texel rim.
/// Boosted mowing scales work with distance in `RunState::cut_rival_if_valid`,
/// so speeds above cruise do not reduce the work applied to a patch of grass.
pub(super) fn expected_cut_for_pass(
    tuning: &VehicleTuning,
    speed: f32,
    offset: f32,
    effective_radius: f32,
) -> f32 {
    let chord = 2.0
        * (effective_radius * effective_radius - offset * offset)
            .max(0.0)
            .sqrt();
    let work_speed = speed.min(tuning.max_speed).max(0.01);
    (tuning.cut_rate_per_second * (chord / work_speed)).clamp(0.0, 1.0)
}

/// Reward expected ownership, retaining a small incentive for partial cutting.
/// Partial work is useful for a later pass but does not earn race score yet.
pub(super) fn claim_reward(cut: u8, owner: u8, expected_cut: f32) -> f32 {
    if owner != 0 || cut >= CUT_COVERAGE_THRESHOLD || !expected_cut.is_finite() {
        return 0.0;
    }
    let remaining = f32::from(CUT_COVERAGE_THRESHOLD - cut) / 255.0;
    let fraction = (expected_cut / remaining).clamp(0.0, 1.0);
    if fraction >= 1.0 {
        1.0
    } else {
        0.2 * fraction * fraction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_crossing_earns_ownership_but_claimed_grass_does_not() {
        assert_eq!(claim_reward(229, 0, 1.0 / 255.0), 1.0);
        assert_eq!(claim_reward(230, 0, 1.0), 0.0);
        assert_eq!(claim_reward(255, 0, 1.0), 0.0);
        for owner in [1, 2] {
            assert_eq!(claim_reward(0, owner, 1.0), 0.0);
            assert_eq!(claim_reward(229, owner, 1.0), 0.0);
        }
    }

    #[test]
    fn slow_cutting_favors_finishing_a_partial_patch() {
        let tuning = VehicleTuning {
            cut_rate_per_second: 1.0,
            ..VehicleTuning::default()
        };
        let cut = expected_cut_for_pass(&tuning, tuning.max_speed, 0.0, tuning.mower_width * 0.5);
        assert!(claim_reward(0, 0, cut) > 0.0);
        assert!(claim_reward(0, 0, cut) < 0.2);
        assert_eq!(claim_reward(220, 0, cut), 1.0);
    }

    #[test]
    fn boost_compensation_preserves_work_per_meter() {
        let tuning = VehicleTuning::default();
        let radius = tuning.mower_width * 0.5 + 1.5 * 15.0 / 512.0;
        for offset in [-0.88, -0.44, 0.0, 0.44, 0.88] {
            let cruise = expected_cut_for_pass(&tuning, tuning.max_speed, offset, radius);
            let boost = expected_cut_for_pass(&tuning, tuning.boost_max_speed, offset, radius);
            assert_eq!(cruise, boost);
            assert_eq!(claim_reward(0, 0, boost), 1.0);
        }
    }

    #[test]
    fn slower_passes_apply_more_work_and_a_miss_applies_none() {
        let tuning = VehicleTuning {
            cut_rate_per_second: 1.0,
            ..VehicleTuning::default()
        };
        let radius = tuning.mower_width * 0.5;
        let fast = expected_cut_for_pass(&tuning, tuning.max_speed, radius * 0.8, radius);
        let slow = expected_cut_for_pass(&tuning, tuning.max_speed * 0.5, radius * 0.8, radius);
        assert!(slow > fast);
        assert_eq!(expected_cut_for_pass(&tuning, 0.0, 0.0, radius), 1.0);
        assert_eq!(
            expected_cut_for_pass(&tuning, tuning.max_speed, radius * 2.0, radius),
            0.0
        );
        assert_eq!(claim_reward(0, 0, 0.0), 0.0);
        assert_eq!(claim_reward(229, 0, f32::NAN), 0.0);
    }
}
