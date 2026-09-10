# Visual polish implementation

The agreed direction is a carefully crafted miniature garden in space. Preserve
the coral/cream mower, leafy lawn, blue-violet meteor stone, and cream/sage UI.

## Scope and acceptance

- [x] Grass: coherent spherical wind with local flutter, clustered blade shape,
  height and color, unchanged root identities and triangle density.
- [x] Mown turf: richer compact stubble and directional sheen; inspect parallel,
  crossing and curved cuts under sunny and shaded lighting.
- [x] Lighting and stone: directional ambient depth, sheltered contact/cavity
  shading, quiet broad stone areas, selective mineral accents and clear craters.
- [x] UI: distinctive licensed display typography, a consistent drawn icon set,
  illustrated presets, integrated coverage/boost, contextual secondary telemetry,
  coherent pause/settings/results styling and gentle milestone/boost feedback.
- [x] Camera: useful editor inspection zoom preserving size comparison; brief,
  skippable arrival and continuous return; reduced-motion support.
- [x] Mower: a readable canopy emblem, stronger material separation and top-visible
  hover lighting without privileging a travel direction.
- [x] Effects: clipping density/size respond to fresh cut amount; restrained rock
  dust, recovery gesture and milestone accents; bounded and never replayed.

## Verification

- [x] Meaningful CPU checks cover new data preparation, bounds, event timing,
  motion reduction, camera transitions and geometry limits.
- [x] Workspace tests, formatting and strict Clippy pass.
- [x] Explicit GPU checks cover all passes, material edges, interaction,
  grass interpolation, sky and bloom.
- [x] Current release inspected in native title/editor/playing/pause/settings,
  including a small window, zoom, arrival and reduced motion.
- [x] Repeatable sunny/shaded/detail/mowing/vehicle captures inspected.
- [x] Warmed GPU workload compared on the same device; regression investigated.
- [x] README/spec updated and final scope audited against current files/results.

Baseline on Apple M2, current Craggy 1280×720, High, 4× MSAA, 120 warmed frames:
279,060 visible tufts; total GPU median 12.779 ms, p95 13.500 ms. This is a local
measurement, not a frame-rate guarantee. Full log: `/tmp/lawn-polish-before-bench.txt`.

Final result: 169 ordinary workspace tests, eight renderer GPU checks, all 14 UI
reference captures, strict workspace Clippy, formatting and the release build
passed. Shipping generation validation passed all 1,000 seeds. Native inspection
also covered the 960×540 content size and a visibly stationary reduced-motion
editor; the original setting was restored and the review game closed afterward.

Final warmed GPU median is 14.519 ms, p95 15.371 ms, an increase of about 1.74 ms
(14%) for the added visuals. CPU encoding median remains 0.172 ms. The targeted
grass simplification retained every feature; its measured difference was within
run-to-run variation. See [PERFORMANCE.md](PERFORMANCE.md) for the full tradeoff,
preparation cost and memory figures. Final log:
`/tmp/lawn-polish-after-final-bench.txt`.

## Implementation evidence

- `grass_roots.rs`, `surface.rs`, and `grass.wgsl`: seeded garden patches,
  three varied leaves per tuft, globe-continuous gusts, short directional stubble,
  and cached cavity factors. Generator roots and terrain/collision data remain
  unchanged. Render roots grow from 20 to 24 bytes; the existing detail fields
  carry terrain shading without another vertex attribute or triangle increase.
- `terrain.wgsl`: directional ambient fill, restrained broad mineral planes,
  selective copper/fracture accents, and soft environment response on enamel.
- `app/garden_ui.rs` and `app.rs`: bundled DM Serif Display under the SIL Open
  Font License, drawn icons and planet presets, coverage dial, segmented boost,
  quieter speed telemetry, paused statistics, and coordinated menu/results cards.
  Settings wrap long bindings and keep Done outside the scrolling content.
- `SceneTransition` and inspection controls: 0.85–1.8× editor zoom, 0.9-second
  arrival/return, input-to-skip, and a continuous spherical camera path. Gameplay
  and mowing stay frozen during arrival. Tests cover opposite hemispheres and
  opposite camera-up directions, plus abandoning a return transition.
- `profile.rs`, `camera.rs`, `renderer.rs`, and UI/effect updates: saved reduced
  motion skips arrivals, holds ambient orbits and decorative clocks, suppresses
  speed shake and UI pulses, and reduces effect motion.
- `mesh.rs`: four-leaf canopy badge, pine trim, bronze bezel and upper cyan pad
  rings. Visible pads and their cosmetic grass-wash sources share an offset.
  Fixed vehicle allocations are 64 KiB vertices and 20 KiB indices.
- `particles.rs`: fresh-cut-area clipping emission, moving rock-contact dust,
  a mint recovery ring and warm milestone motes. Simulation-time consumption
  prevents duplicate events; catch-up batches and total storage remain bounded.

## Repeatable visual references

The renderer's `gpu_smoke` test accepts `LAWN_CAPTURE_DIR` and
`LAWN_CAPTURE_VIEW`. Reviewed views include `day`, `night`, `detail`, `vehicle`,
`recovery`, `impact`, and `milestone`. Actual mowing stamps produce the
`stripes-day`, `stripes-night`, `crosscut-day`, `crosscut-night`, `curve-day`, and
`curve-night` references; their cut direction and coverage use the gameplay path.
Reference captures render all roots and are not performance measurements.

`gpu_capture_garden_ui_references` renders the real egui shapes and font atlas
for title, editor, gameplay, pause, settings, scrolled settings, and results at
960×540 and 1280×720. It verifies critical headings/actions fit after layout
settles and never writes a user profile. The backdrop is deliberately plain so
the captures isolate UI layout; native inspection checks the combined scene.

Commands are documented in [README.md](README.md). Local inspection artifacts
are in `/tmp/lawn-polish-after`, `/tmp/lawn-effects-review`, and
`/tmp/lawn-ui-polish`.
