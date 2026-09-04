# Lawn Orbit

Lawn Orbit is a complete tiny-planet hover-mowing game built directly in Rust,
`wgpu`, WGSL, Rapier, winit, and egui. It implements
[`spec/game.md`](spec/game.md): deterministic fuzzy spherical worlds, seam-safe
authoritative mowing, arcade hover driving, Standard jobs, Free Mow, a first-play
tutorial, local records, accessibility settings, and keyboard/gamepad input.
The shipping planets are 15-meter-radius lawn globes covered in exaggeratedly
tall geometric grass and viewed from an undistorted top-down camera.

No reusable game engine is used. Gameplay simulation is headless and independent
of the renderer, which keeps world generation, mowing, scoring, and vehicle rules
deterministic and directly testable.

## Build and run

The workspace requires Rust 1.93 or newer and a desktop GPU/driver supported by
`wgpu` (Metal, Vulkan, or Direct3D 12). From the repository root:

```sh
cargo run --release -p lawn_orbit
```

The first launch opens the title screen. Choose the fixed tutorial seed, enter a
hexadecimal/decimal/phrase seed, or create a random Standard or Free Mow planet.
Profiles and seed-specific records are saved atomically in the operating system's
per-user application-data directory.

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
| Rotate / zoom top-down camera | I/J/K/L or right-drag | Right stick |
| Recenter camera | C | Right-stick click |
| Pause | Escape | Start |
| Submit completed job | Enter | South button in the prompt |
| Diagnostics | F3 | — |

Movement bindings can be remapped in Settings and analog values are preserved.
The mower deck is always active directly beneath the chassis whenever it is grounded.
The settings screen also contains horizontal movement sensitivity/inversion, camera shake,
camera auto-follow, field of view, motion reduction, boost disable, grass density,
render scale, MSAA, fullscreen, and a high-contrast coverage overlay.

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `lawn_core` | Versioned generation, cube-sphere math, validation, fixed-step simulation, Rapier vehicle, camera, mowing state, jobs, scoring, flow, and profiles |
| `lawn_render` | `wgpu` terrain/vehicle/grass rendering, interaction compute field, shadows, clipping particles, HDR composite, culling, dirty texture uploads, and diagnostics |
| `lawn_orbit` | Desktop lifecycle, menus/HUD/results, input and rumble, and atomic persistence |
| `lawn_tools` | Headless seed inspection, generation timing, and deterministic validation batches |

Runtime tuning lives in [`config/game.ron`](config/game.ron). The external file is
parsed and validated at startup, while tests assert that it matches the documented
shipping defaults. Rendering degrades by capability and quality preset; the
baseline path requires no optional GPU feature and uses a storage-buffer grass
interaction fallback.

## Verification

Run the complete test, lint, release-build, and generator checks with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
cargo run --release -p lawn_tools -- validate 1000
```

The test suite exercises deterministic generation (including compact grass-root
records), arbitrary coordinate poles, cube-face seams and corners, maximum-speed
mowing sweeps, weighted coverage, hover stability, circumnavigation, recovery,
fixed-step equivalence at 30/60/120/240 Hz, tutorial flow, scoring, persistence,
input behavior, and parse/validation of every shipping WGSL module. It runs without
opening a window or creating a GPU device except for the manual application smoke
test.

Useful headless commands:

```sh
# Shipping-resolution generation and validation; roots are skipped because they
# are cosmetic and do not affect playability.
cargo run --release -p lawn_tools -- validate 1000

# Reduced-resolution fast validation beginning at deterministic stream index 500.
cargo run --release -p lawn_tools -- validate 1000 500 --quick

# Full shipping planet, including grass roots, as a JSON report.
cargo run --release -p lawn_tools -- inspect "cozy planet"
```

`validate` exits unsuccessfully if any seed fails. Its JSON report includes the
generator version, failure details, generation-attempt histogram, mowable and
reachable bounds, and median/p95/maximum generation times. F3 in the game exposes
frame p50/p95, draw visibility, grass/triangle counts, texture uploads, particles,
CPU encoding time, graphics tier, adapter, seed, and deterministic planet hash.
