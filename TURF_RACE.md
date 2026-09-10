# Turf Race

The world editor now defaults to a race against a blue AI mower. Free Mow remains
available beside it. Both modes use the exact planet shown in the editor.

## Match rules

- Each patch belongs permanently to the mower that first finishes cutting it.
  Crossing an opponent's cut turf cannot steal it or score it again.
- Scores measure actual mowable surface area, including the cube-sphere area
  weights. Rocks do not count toward the lawn or either score.
- The first mower strictly past 50% wins immediately. Exactly 50% is not a win;
  an exhausted lawn split equally is a draw. There is no countdown or time limit.
- Winning starts a victory lap at the player's current position and speed.
  The rival bursts into colorful fragments and disappears; the player keeps
  driving and cutting while the victory card is open. Keep mowing dismisses
  the card, and the final race shares, time, and bumps remain fixed.
- The victory lap shows total lawn coverage and supports finishing the rest of
  the grass. Losses and draws stop at the results screen. Rematch regrows the
  same world and resets both mowers, ownership, boosts, elapsed time, and bumps.
- The top bar shows player coral, rival blue, and the remaining unclaimed grass.
  Subtle turf tints and a rival marker make positions and routes readable.

## Rival and bumpers

The rival drives a real hover vehicle using the same controller, boost, cutting,
terrain collision, and recovery rules as the player. A small, seam-connected
surface graph finds reachable fresh grass around the rock. It replans as the
lawn changes, slows for direction changes, and boosts on clear productive runs.
It favors nearby useful routes, with a small preference for contested meadows.

Mower contact produces a symmetric shove along the planet's surface. Swept
contact catches fast crossings; separation follows the curvature and preserves
the hover height. Steering remains available during a short, 0.28-second impact
response, then returns to full strength. There is no damage or lost turf.
Localized dust and a brief controller pulse make meaningful impacts visible.

Each mower retains its existing Rapier terrain simulation. A deterministic
arcade contact solver couples the two bodies at the fixed 120 Hz simulation
rate; it does not depend on renderer timing or GPU readback.

## Implementation cost

Race ownership adds an optional byte per mowing cell (1.5 MiB at shipping
resolution). The packed four-byte authoritative mowing cell remains unchanged,
and cached area totals make score queries constant time. Ownership travels with
the existing dirty tiles through a reusable staging buffer. The renderer reuses
the mowing texture's otherwise unused recent-cut byte for race ownership.

The rival adds a fixed 64 KiB vertex buffer, a world and shadow draw, and two
vectors in the frame uniform. Both vehicles share the existing interaction
field and bounded particle pool. Navigation uses 3,456 nodes and replans about
every 0.65 seconds. These are bounded resource changes, not a measured frame-time
guarantee.

## Acceptance evidence

- All 212 ordinary workspace tests pass, with formatting, strict workspace
  Clippy, and the complete release build.
- Headless AI matches finish on open and rocky worlds. The unattended rival
  reached a majority in 136.9 seconds at resolution 128 and 155.7 seconds on
  rocky seed 7 at resolution 256. A separate shipping-resolution 512 endurance
  check finished in 159.0 seconds. These cases needed no recovery or airborne
  travel; they are samples, not an exhaustive assessment of difficulty.
- Ownership tests cover cut thresholds, permanent claims, area-weighted totals,
  cube seams/corners, rocks, compatible snapshots, and dirty tile staging.
  Match tests cover both winners, strict majority, draws, loss/draw freezing,
  pause, restart, and switching back to Free Mow.
- Victory tests cross the actual majority threshold while driving, then verify
  continued movement and cutting, unchanged final results and ownership, a
  single rival defeat, global coverage milestones, and a clean rematch.
  App checks exercise the real victory controls, dismissal, reopening, held
  input, and gamepad pause navigation.
  Particle regressions retain the defeat through skipped surface frames and
  pause, then emit it exactly once without replaying unrelated effects.
- Real player chase input starts the mowers 4.27 metres apart and contacts the
  active AI after 0.17 seconds. Twelve seconds of driving produces 26 bumps with
  matching positive impact events. Both finish grounded, with exactly identical
  poses, ownership, scores, and events at 30/60/120/240 display Hz.
- Independent contact tests cover momentum transfer, separation, high-speed
  crossing, arbitrary hemispheres, and return of steering control.
- All ten explicit renderer GPU checks pass on Apple M2, including race,
  Free Mow, and the victory burst at supported 1×/2×/4× MSAA counts. Pixel
  checks locate both mower palettes and defeat particles after rival removal.
  Race, bumper, and normal/reduced victory reference images were inspected.
- The UI GPU check passes all 28 references at 960×540 and 1280×720, including
  race HUD, pause, each outcome, and the open/dismissed victory card.
  Main-menu, editor, and Free Mow checks remain covered.
- Initial race native release inspection covered both window sizes, mode
  selection, the moving rival and its near/far-side marker, pause/resume, a complete AI win,
  rematch, and switching to solo play. On the default Classic planet
  `0x4C41574E4F524249`, the unattended rival won at 02:37.0. Rematch reset both
  shares to zero and regrew the same world; Free Mow restored its solo HUD and
  removed the rival. The victory lap was verified through simulation/input
  regressions and actual GPU/UI captures; a native player victory was not
  manually played through.

Useful focused checks:

```sh
cargo test -p lawn_core --test turf_race -- --nocapture
cargo test -p lawn_core --test turf_race unattended_rival_wins_rocky_world_at_shipping_512 -- --ignored --nocapture
cargo test -p lawn_render gpu_ -- --ignored --nocapture --test-threads=1
LAWN_CAPTURE_DIR=/tmp/lawn-race LAWN_CAPTURE_VIEW=race cargo test -p lawn_render gpu_smoke_race -- --ignored --nocapture
LAWN_CAPTURE_DIR=/tmp/lawn-race LAWN_CAPTURE_VIEW=bumper cargo test -p lawn_render gpu_smoke_race -- --ignored --nocapture
LAWN_CAPTURE_DIR=/tmp/lawn-victory LAWN_CAPTURE_VIEW=victory cargo test -p lawn_render gpu_smoke_victory -- --ignored --nocapture
LAWN_CAPTURE_DIR=/tmp/lawn-victory LAWN_CAPTURE_VIEW=victory-reduced cargo test -p lawn_render gpu_smoke_victory -- --ignored --nocapture
LAWN_UI_CAPTURE_DIR=/tmp/lawn-race-ui cargo test -p lawn_orbit gpu_capture_garden_ui_references -- --ignored --nocapture
```

This is one opponent with one difficulty. Further pacing, route quality, and
bumper strength should be tuned from hands-on matches. Online multiplayer,
weapons, power-ups, ratings, and progression are outside this pass.
