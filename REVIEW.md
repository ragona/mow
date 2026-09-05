# Correctness and performance review

Review baseline: `5631b86` on `main` (2026-09-04). The baseline passed 77 tests.
Reviewed all four crates, shaders, configuration, and the gameplay specification,
with independent second passes over the changes. All actionable findings below
are fixed; no known review finding remains open.

## Resolved findings

| Area | Finding and fix | Verification |
| --- | --- | --- |
| Mowing | Recovery stamped the entire teleport path. Recovered ticks now skip cutting and recording. | Teleport regression preserves every mowing cell. |
| Mowing | Center-face offset sampling missed texels near cube corners. Each intersected face now uses conservative spherical-cap bounds and dot-product inclusion. | Exhaustive footprint comparisons across faces, seams, corners, and cap sizes. |
| Mowing | Small increments rounded to zero forever, including subdivided sweeps. Fractional progress now accumulates and survives snapshots. | Slow-cut, grouping, long-sweep, legacy-snapshot, and invalid-snapshot regressions. |
| Mowing performance | Each footprint hashed candidates and evaluated per-cell inverse trigonometry. Dirty tiles used hashing and repeated key allocation; unchanged packed cells were uploaded again. Removed this work and retained staging capacity. | Exhaustive footprint checks, unchanged-upload regression, shipping-resolution benchmark. |
| Locator | HUD/render requests flood-filled the 1.57-million-cell field repeatedly each frame. Results are cached until coverage changes, with active-run refresh capped at four times per second. | CPU benchmark exercises cold and unchanged-field requests. |
| Simulation | Catch-up omitted discarded time and retained a stale whole tick; float accumulation drifted during long runs. Account for all dropped ticks and derive elapsed time from integer ticks. | Catch-up accounting, long-session timer, and 30/60/120/240 Hz equivalence tests. |
| Vehicle | Slow travel rounded to zero; opposing orientations produced invalid interpolated axes. Use stable angular distance and derive axes from quaternion interpolation. | Tiny-displacement, opposite-pose, driving, recovery, and circumnavigation tests. |
| Physics | High servo gains overshot, radial requests affected surface drive, distant pad hits allowed airborne mowing, and invalid timesteps could corrupt state. Bound the integrated servo, project drive tangentially, check pad distance, and ignore invalid timesteps. | High-gain, airborne, invalid-timestep, and hover-stability regressions. |
| Camera and paths | Opposite headings could remain stuck; near-antipodal sweeps used the wrong plane; recorder coalescing erased turns, long arcs, and return trips. Corrected rotation and coalescing geometry. | Look-behind, near-antipodal endpoint/plane, route and out-and-back tests. |
| Scoring | Completed runs could be submitted repeatedly. Submission now requires an active run. | Submission-once regression. |
| Input timing | High-refresh frames lost mouse movement and catch-up frames repeated it. Consume transient input only when a fixed tick runs. | Split-versus-batched frame tests compare camera states exactly. |
| Input lifecycle | Focus loss, consumed releases, disconnected controllers, and multiple pads could leave controls stuck. Track controller state separately and consistently clear/release controls. | Focus, controller release/disconnect, analog remapping, and menu-key tests. |
| Menus | Gamepad key presses lacked releases and gameplay buttons could activate the HUD. Generate complete menu pulses, separate gameplay input, and use a real modal for discard confirmation. | Menu event regressions and desktop smoke check. |
| App lifecycle | Hidden windows polled continuously, failed surface acquisition retried immediately, and asynchronous loading could resume an unfocused game. Sleep while hidden, throttle retries, and preserve the paused state. | Focus and real-worker completion regressions; desktop pause check. |
| Persistence | Loading a corrupt or future profile fell back to defaults that overwrote the original on exit. Preserve the original and show a session-only persistence notice. | Invalid/future-file tests verify unchanged bytes after attempted save. |
| Persistence | Concurrent saves shared one staging filename; failures left files behind and data was not flushed. Use exclusive unique staging files, flush data/directory, atomically rename, and clean up failures. | Eight concurrent writers, round-trip, and failed-rename cleanup tests. |
| Settings | NaN/infinite settings survived clamps; older bindings could omit newer actions. Repair invalid values and records, fill missing actions, preserve explicit remaps/unbindings, and bound recent history. | Migration and invalid-settings/record tests. |
| Configuration | Invalid ranges, zero divisors, nonfinite values, or excessive resolutions/density could panic or poison geometry. Validate generator, vehicle, and job domains before startup; cap the actual root allocation before generating cosmetics. | Invalid-config/allocation tests plus editor endpoint combinations and full-density large worlds. |
| Generation performance | Failed attempts still generated cosmetics, clearance was flood-filled twice, interior neighbors used cube projection, irrelevant mountain detail was evaluated everywhere, and root buffers could double after underreservation. Skip/reuse this work and reserve per-cell rounded-up root counts. | Seed validation, deterministic before/after comparison, generation benchmarks. |
| Grass bounds | A single corner underestimated patch extent; base-radius horizon culling hid elevated grass. Bound all patch corners and account for terrain depressions, elevated roots, and blade height. | Partial-tile containment and horizon tests. |
| Geometry | Inward triangle winding broke near-side shadow casting. Correct winding and cull solid backfaces. | Triangle-orientation regressions and real GPU passes. |
| GPU capability and bounds | MSAA requests could exceed color/depth/resolve support, while fixed shadow/far-plane bounds clipped larger worlds. Select supported sample counts and derive bounds from world size. | Capability-matrix and shadow-volume tests; 1×/2×/4× GPU execution. |
| Grass interaction | Elevated source positions disagreed with the base-sphere compute field; reversed `smoothstep` edges were undefined. Use common coordinates, defined falloff, and cheap out-of-radius rejection. | Elevated-source regression and real compute execution. |
| Rendering performance | Vehicle/ring/source and particle vectors allocated every frame, invariant indices were reuploaded, and transient attachments were stored unnecessarily. Reuse buffers, upload indices once, and discard transient MSAA/depth stores. | Capacity/topology regressions, GPU smoke, desktop diagnostics. |
| Rendering correctness | Composite coverage was multiplied twice, darkening silhouettes; visual time advanced too quickly above 240 FPS; pausing rewound interpolation; GPU timing ring wrap could report older data. Correct compositing, elapsed-time handling, paused poses, and readback ordering. | High-refresh presentation tests, GPU timestamp/readback smoke, desktop rendering. |
| Tools and documentation | `validate 100 --quick` failed, unknown/extra arguments were ignored, and enormous counts panicked on allocation. Parse flags independently, reject mistakes, and use fallible reservation. Document that gameplay tuning is embedded at build time. | Argument tests and direct CLI checks. |

## Validation

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo test --workspace` — 126 passed; two explicit opt-in tests skipped by default.
- `cargo build --release --workspace` and the performance example — passed.
- Explicit GPU smoke — passed on Apple M2 with 5,305 grass roots, particles,
  compute, shadow, world, composite, timestamps, and pixel readback at 1×/2×/4× MSAA.
- Editor tests cover 144 endpoint/seed combinations plus the largest worlds with
  shipping-resolution grass roots. The standard suite also validates 1,000
  arbitrary seeds at test resolution.
- Release validation passed all 1,000 shipping-resolution seeds. Comparison of
  27 complete shipping worlds against the baseline preserved every reported
  content field and deterministic hash. Direct CLI checks verified optional
  flag ordering and clean errors for excess arguments and oversized counts.
- Desktop smoke — title screen, live Meadow preview, entering the prepared
  planet, active mowing, diagnostics, and pause rendered successfully. Observed
  frame median was approximately 16.7 ms; this is a sample, not a frame-time bound.

## Performance measurements

Sequential release runs on the same Apple M2, after compilation and the desktop
test had stopped. These are local observations, not guaranteed performance bounds.
The gameplay benchmark uses the same 3,600 input ticks and extracts dirty uploads
every other tick against both versions.

| Workload | Baseline | Reviewed code |
| --- | ---: | ---: |
| Generate 1,000 shipping planets without cosmetic roots: total | 28.485 s | 14.345 s |
| Generation median / p95 per planet | 28.187 / 33.296 ms | 14.255 / 15.714 ms |
| 3,600 gameplay ticks plus dirty upload extraction: total CPU | 1,067.47 ms | 116.26 ms |
| Gameplay tick median / p95 | 360.79 / 418.96 µs | 28.71 / 59.04 µs |
| Remaining-grass locator, first request | 4.756 ms | 2.447 ms |
| Remaining-grass locator, unchanged-field request | 4.776 ms | <0.001 ms |
| Gameplay preparation, including physics and mowing allocation | 80.63 ms | 85.67 ms |

Generation took approximately half the total time; the fixed gameplay workload
used approximately 9.2 times less CPU time. The small preparation increase
includes allocation of fractional cut residuals. The final drive coverage differs
(14.16% to 15.32%) because this comparison includes the corrected mowing footprint,
cut accumulation, camera, and physics behavior, rather than an isolated algorithm
microbenchmark.

Reproduce the current workloads with:

```sh
cargo run --release -p lawn_tools -- validate 1000
cargo run --release -p lawn_tools --example performance
```

For a baseline comparison, build the same performance example against `5631b86`.

## Tradeoffs and coverage limits

Fractional mowing adds one `f32` per cell (6 MiB at the shipping 512-face
resolution). This prevents lost progress without per-cell heap objects or hashing;
legacy snapshots remain readable. Conservative grass culling favors retaining
visible elevated blades over the most aggressive possible rejection.

Validation ran on macOS/Apple M2 with Rust 1.93.1. Windows/Linux drivers and physical
gamepad hardware were not exercised here; controller behavior is covered by
synthetic input tests. Passing these checks does not establish flawless operation
on every machine or for every future change. The GPU smoke and repeatable CPU
example are included for continued verification.

## Golden-hour garden presentation follow-up

The visual pass adds warm directional lighting, cool ambient shadows, distinct
enamel/rock/rubber materials, a rounded mower and antenna, leafy grass and brushed
stubble, bounded clipping fans, a pastel sky and atmospheric rim, and matching
cream/coral menus. Bloom uses persistent quarter-resolution targets, retains
pipelines during resize, and skips both its passes and composite sample on Low.

Review and live inspection also resolved these presentation issues:

- Unbounded hover displacement stretched blades into radial spokes. Bending now
  stays within each blade's length and lowers its tip.
- Cutting extremely short custom grass could increase its height. Stubble and
  comb bend scale down together.
- Clipping density depended on display rate; emission now follows simulation
  time with bounded work after a long frame.
- Paused particle tumble and chassis springs continued moving. Both now freeze.
- Per-blade noise and pale highlights obscured form; coherent green patches and
  darker roots improve material depth and cut-path contrast.

Validation: 129 ordinary workspace tests passed; the real GPU smoke test passed
at 1×/2×/4× MSAA on Apple M2; and the new bloom readback test passed for threshold,
HDR preservation, two-axis spread, hue, stale-frame clearing, and tiny/odd resizes.
Formatting, strict workspace clippy, and release builds passed. Native inspection
covered the title composition and active mowing with visible cut paths and
clippings. The live High-quality title scene with 4× MSAA showed approximately
16.7 ms median frame time on this M2; this is an observation, not a cross-device
performance guarantee. The mower remains within the existing GPU buffers at
858 vertices and 2,544 indices, and the visual changes do not alter generation,
physics, mowing coverage, or saved-world semantics.

## Rotor wash and shaded-side readability follow-up

- Fixed a stop-time direction flip: normalizing tiny residual tangent velocities
  gave suspension jitter a full-strength directional wake. Travel bias and wake
  offset now fade continuously below 2 m/s, leaving broad outward downwash.
- Softened each source's central core, removing the arbitrary full-strength kick
  when its center crosses a field sample. Coherent outward pressure ripples and
  a small swirl keep stationary hovering lively.
- Replaced the underdamped Euler spring with an exact critically damped step,
  avoiding backward rebound and keeping release consistent across frame rates.
- Interpolated the displacement field around texel centers, including unfolded
  cube edges and three-face corners. Root vertices skip those extra reads.
- Replaced the blade's hard lean clamp with a smooth limit, preserving length
  while allowing stronger downwash without a rigid flat inner disc.
- Raised ambient fill by up to 85% on unlit terrain and grass, tapering away in
  direct sunlight to retain the golden-hour contrast.

Validation: 131 ordinary tests passed, strict workspace clippy and formatting
passed, and all four explicit GPU checks passed on Apple M2. New readbacks cover
near-center force continuity, finite tangent displacement, bounded strong
impulses, monotone release and matching trajectories at 30/60/120 Hz, and 4,200
interpolation probes across interiors, every cube edge, and all eight corners.
The complete render smoke test still passes at 1×/2×/4× MSAA.
The release build passed. A final native visual check was unavailable because
the Mac was locked; the numerical interaction and shader checks above completed
without a window server.

## Smooth rock boundaries and stone detail

- Replaced nearest-cell, flat triangle material IDs with a shared continuous
  coverage field. A local one-cell-radius filter rounds stair steps, and a
  narrow fragment threshold keeps the visible grass/rock color edge crisp.
- Grass tapers to that same contour and collapses on its rock side, avoiding
  tall tufts over the exposed stone. Its cached fringe weight adds four bytes
  to the GPU instance record; generator roots and patch indices stay unchanged.
- Refined shipping render terrain from 64 to 128 subdivisions, welded cube
  edges/corners, and derived normals from the rendered triangles. Custom
  resolutions above 128 retain their original density. This adds 147,456 static
  terrain triangles at shipping settings without changing collision geometry.
- Removed interpolated random rock colors in favor of restrained world-space
  mineral planes, seams, and distance-filtered grain.

Validation: 139 ordinary tests and all five explicit GPU checks passed on Apple
M2, along with strict workspace clippy and formatting. New checks cover a closed
terrain mesh, shared seam normals, bounded refinement, continuous material
coverage, unchanged generated data, and matching grass-fringe coverage. A GPU
readback verifies that a single triangle contains both materials with a narrow
antialiased transition. The complete render smoke still passes at 1×/2×/4× MSAA.
The release build passed, and the final contour was inspected in the live Craggy
preview and game. That scene showed 16.71 ms median / 17.68 ms p95 frame time on
this M2 with approximately 325,000 visible tufts; these are local observations.

## Overgrown asteroid art pass

- Replaced chalky stone with matte blue-violet meteor facets, pale fractures,
  sparse copper inclusions, and moss in crevices near the lawn. Fine features
  use pixel-footprint filtering; the material adds no texture fetches or loops.
- Added deterministic shallow impact bowls and chipped raised rims to exposed
  rock. A conservative spherical lookup limits crater work to nearby candidates
  during mesh preparation. Relief vanishes before the grass fringe; generated
  worlds, root identity, collision, mowing, and saves keep their original data.
- Replaced the screen-space dusk backdrop with a world-anchored nebula and star
  panorama. The 1024-pixel cube faces contain indigo, violet, teal and warm dust,
  5,000 varied stars, rare glints, and a cratered companion moon that occludes
  stars. Linear-light mip generation keeps small stars stable through zoom.
- The sky bakes once using a bounded set of startup workers, occupies 32 MiB
  including mipmaps, and needs one filtered lookup for visible sky pixels.
  Resize and planet edits reuse it. A quiet Apple M2 release bake took 661 ms.
- Added repeatable sunny, shaded, close-up, and moon reference captures using
  the actual render passes. Inspection led to sharper stars, deeper matte stone,
  larger legible craters, and restrained rims rather than blurry pale patches.

The terrain vertex stride grows from 32 to 48 bytes for cached crater masks,
bringing shipping static terrain buffers to 6.75 MiB without extra triangles.
The fixed mower vertex allocation grows to 48 KiB to retain its 1,024-vertex
capacity. Crater placement and sky baking do no per-frame CPU work.

Validation: 145 ordinary workspace tests and all six explicit GPU checks passed
on Apple M2, along with formatting and strict workspace Clippy. Crater checks
cover deterministic placement, real bowl/rim geometry, unchanged grass, bounded
relief, tiny valid worlds, and spatial pruning at cube seams. New sky readbacks
exercise 182 directions across all cube faces, edges and corners, two camera
origins, and both gamma and sRGB framebuffer paths (728 comparisons). Existing
wash, grass interpolation, material-edge, bloom, and 1×/2×/4× MSAA checks pass.
The release build passed. Native inspection covered the title, live Craggy
editor, and active mowing with visible clippings. UI-driven scenes showed
16.6–16.7 ms median frames and 17.7–33.6 ms p95 with roughly 323,000–368,000
visible tufts on this M2; these are local observations, not an isolated benchmark
or a guarantee of a locked frame rate. All-roots reference captures deliberately
skip gameplay culling and should not be used as frame-time benchmarks.
