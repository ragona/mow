# Lawn Orbit

Lawn Orbit is a tiny-planet hover-mowing sandbox built directly in Rust,
`wgpu`, WGSL, Rapier, winit, and egui. It implements
[`spec/game.md`](spec/game.md): deterministic fuzzy spherical worlds, seam-safe
authoritative mowing, nimble omnidirectional hover driving, a world editor, an
unscored mowing sandbox, a first-play tutorial, accessibility settings, and
keyboard/gamepad input. Planets can range from compact meadows to larger rocky
worlds and are viewed from an undistorted, mostly top-down chase camera.

The visual style is a cozy garden growing on a weathered meteor: leafy grass,
warm sunlight and cool shadows, blue-violet stone, and a coral-and-cream mower.
Shallow craters, chipped rims, pale fractures, copper inclusions, and mossy
crevices give the exposed rock character. A textured nebula sky, varied stars,
and a little companion moon surround the garden as the camera orbits.
Mowing leaves short brushed stubble and a small fan of tumbling clippings. Soft
HDR bloom highlights the hover engines on Standard and High quality; Low skips
the bloom passes. The cream-and-sage interface keeps the planet in view.
Rock edges use a continuous material contour shared with a short grass fringe,
and the stone has crisp mineral detail that fades at distance to avoid shimmer.
Crater relief is prepared with the static render mesh and fades out before the
lawn; it does not change generated worlds, driving, mowing, or saved seeds.

No reusable game engine is used. Gameplay simulation is headless and independent
of the renderer, which keeps world generation, mowing, scoring, and vehicle rules
deterministic and directly testable.

## Build and run

The workspace requires Rust 1.93 or newer and a desktop GPU/driver supported by
`wgpu` (Metal, Vulkan, or Direct3D 12). From the repository root:

```sh
cargo run --release -p lawn_orbit
```

The first launch opens the title screen. Open the world editor to choose a terrain
preset or tune planet size, rockiness, peak count, peak height, and rolling terrain.
The planet updates beside the controls as you edit, including when you change its
seed. Rockiness can be set to 0% for a fully grassy world without outcroppings;
the Meadow preset starts there. Enter a hexadecimal, decimal, or phrase seed,
then choose Start Mowing to enter the exact planet shown in the preview.
Settings plus favorite and recent seeds are saved atomically in the operating
system's per-user application-data directory.

On Linux, the platform libraries required by winit and gilrs must be installed;
package names vary by distribution and commonly include Wayland or X11 and udev
development packages.

## Default controls

| Action | Keyboard / mouse | Gamepad |
| --- | --- | --- |
| Move freely | WASD or arrow keys | Left stick (triggers also supported) |
| Boost | Space | South button |
| Look behind | Q | North button |
| Recover | Hold R | Hold East button |
| Rotate / zoom chase camera | I/J/K/L or right-drag | Right stick |
| Recenter camera | C | Right-stick click |
| Pause | Escape | Start |
| Diagnostics | F3 | — |

Movement bindings can be remapped in Settings and analog values are preserved.
The mower deck is always active directly beneath the chassis whenever it is grounded.
The settings screen also contains horizontal movement sensitivity/inversion, camera tilt,
camera shake, camera auto-follow, field of view, motion reduction, boost disable, grass density,
grass height, render scale, MSAA, fullscreen, and a high-contrast coverage overlay.

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `lawn_core` | Versioned generation, cube-sphere math, validation, fixed-step simulation, Rapier vehicle, camera, mowing state, sandbox flow, and profiles |
| `lawn_render` | `wgpu` terrain/vehicle/grass rendering, interaction compute field, shadows, clipping particles, HDR composite, culling, dirty texture uploads, and diagnostics |
| `lawn_orbit` | Desktop lifecycle, world editor, menus/HUD, input and rumble, and atomic persistence |
| `lawn_tools` | Headless seed inspection, generation timing, and deterministic validation batches |

Gameplay tuning lives in [`config/game.ron`](config/game.ron), which is embedded
at build time and parsed and validated at startup. Rebuild after editing it.
Tests assert that it matches the documented shipping defaults. Rendering degrades
by capability and quality preset; the baseline path requires no optional GPU
feature and uses a storage-buffer grass interaction fallback.

## Verification

Run the complete test, lint, release-build, and generator checks with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
cargo run --release -p lawn_tools -- validate 1000
# Requires an available GPU; executes every rendering pass and reads pixels back.
cargo test -p lawn_render gpu_smoke -- --ignored --nocapture
# Verifies bloom thresholding, blur, HDR color, and resize behavior by readback.
cargo test -p lawn_render gpu_bloom -- --ignored --nocapture
# Includes hover wash, cube-edge interpolation, and the grass/rock material edge.
cargo test -p lawn_render gpu_ -- --ignored --nocapture
# Optional repeatable 1024×768 art reference; writes PPMs at each supported MSAA count.
# LAWN_CAPTURE_VIEW also accepts night, detail, or moon. Captures draw all roots,
# so their one-frame GPU timings are not representative gameplay benchmarks.
LAWN_CAPTURE_DIR=/tmp/lawn-art LAWN_CAPTURE_VIEW=day cargo test -p lawn_render gpu_smoke -- --ignored --nocapture
```

The test suite exercises deterministic generation (including compact grass-root
records), arbitrary coordinate poles, cube-face seams and corners, maximum-speed
mowing sweeps, weighted coverage, hover stability, circumnavigation, recovery,
fixed-step equivalence at 30/60/120/240 Hz, tutorial flow, scoring, persistence,
input behavior, and parse/validation of every shipping WGSL module. The default
suite runs without opening a window or creating a GPU device. The explicitly
requested GPU smoke test requires a graphics adapter but no window server.

Useful headless commands:

```sh
# Shipping-resolution generation and validation; roots are skipped because they
# are cosmetic and do not affect playability.
cargo run --release -p lawn_tools -- validate 1000

# Reduced-resolution fast validation beginning at deterministic stream index 500.
cargo run --release -p lawn_tools -- validate 1000 500 --quick

# Full shipping planet, including grass roots, as a JSON report.
cargo run --release -p lawn_tools -- inspect "cozy planet"

# Warmed offscreen GPU workload with shipping camera, density and culling.
# Uses 60 warmup frames then 120 samples. Run on an otherwise idle GPU.
LAWN_BENCH=1 cargo test --release -p lawn_render gpu_smoke -- --ignored --nocapture --test-threads=1
# LAWN_BENCH_SCENE=meadow or mown; LAWN_BENCH_QUALITY=low;
# LAWN_BENCH_WIDTH=2560 LAWN_BENCH_HEIGHT=1440 for a larger framebuffer.

# Fixed 30-second gameplay workload, dirty uploads, and remaining-grass locator.
# Compare timings on the same idle machine; results are not pass/fail thresholds.
cargo run --release -p lawn_tools --example performance
```

`validate` exits unsuccessfully if any seed fails. Its JSON report includes the
generator version, failure details, generation-attempt histogram, mowable and
reachable bounds, and median/p95/maximum generation times. F3 in the game exposes
frame p50/p95, draw visibility, grass/triangle counts, texture uploads, particles,
CPU acquisition/encoding time, measured GPU frame time, graphics tier, adapter,
seed, and deterministic planet hash. GPU pass spans can overlap and must not be
added together.

The latest correctness/performance findings and validation results are recorded
in [`REVIEW.md`](REVIEW.md). The subsequent measured performance investigation,
including before/after results and rejected experiments, is in
[`PERFORMANCE.md`](PERFORMANCE.md).
