# Browser performance pass

Measured September 10, 2026 on an Apple M2, using optimized native Metal and
the Codex in-app browser's `BrowserWebGpu` backend. The browser preserves the
native rendering features and quality. These are local observations, not a
cross-browser performance guarantee.

## Matched workload

The shared benchmark uses seed 42 and the same Craggy generator settings, camera,
High quality, full grass density, render scale 1.0 and 4× MSAA. Resolution means
physical render pixels: 1280×720 or 2560×1440, independent of CSS size or display
scale. The stationary scene draws 284,324 visible tufts and 5,315,520 triangles;
the scripted race finishes at 244,831 tufts and 4,606,174 triangles, with 224
clipping particles. Those counts match between native and browser.

Each run settles the simulation, warms 90 rendered frames, then measures 360.
The race advances one 1/120-second tick per rendered frame; the 360 measured
frames cover three simulation seconds. A live 60 FPS game normally advances
about two physics ticks per frame, so this is a matched renderer/script workload,
not a full-game physics-throughput test. Wind, particles and other visual
animation also use the explicit fixed delta. The benchmark reads and writes no
profile. It excludes startup, egui/HUD and compositor display timing.

Version 2 drains queued GPU work before and after measurement. Completed FPS
therefore includes CPU work, frame scheduling, GPU execution and completion
callback delivery. It is not display-flip FPS. `frame_ms` separately measures
redraw callback intervals; those alone can conceal queued GPU work. GPU spans
are asynchronously returned latest samples, potentially reused across sampled frames,
and should not be interpreted as uniquely correlated per-frame measurements.

## Retained renderer measurements

| Workload | Native completed FPS | Browser completed FPS | GPU timing |
| --- | ---: | ---: | --- |
| 720p stationary | 60.0 | 59.8 | On |
| 720p scripted race | 60.0 | 59.9 | On |
| 1440p stationary | 59.3 | 52.0 | Off |
| 1440p scripted race | 59.8 | 59.2 | Off |

The 720p workloads and 1440p race approach the 60 Hz scheduling ceiling in this
environment. The dense 1440p stationary view retains a meaningful gap. With GPU
timestamps enabled, median stationary GPU spans are 13.9 ms native / 14.0 ms
browser at 720p and 16.2 ms / 16.4 ms at 1440p. Similar median spans suggest costs
outside the measured GPU span may contribute. Browser command processing,
presentation and scheduling remain plausible contributors; reused asynchronous
samples and medians do not isolate the cause or rule out GPU timing tails.

These are short runs, with normal scheduling variation. A separate unprofiled
720p browser run returned 57.5 FPS; profiler-on results are not evidence that
profiling improves performance. At 1440p stationary, the unprofiled run was
52.0 FPS versus 50.8 FPS with timestamp collection. Avoid interpreting small
differences as reliable speedups. Raw selected observations are retained in
[`performance/browser-parity-m2.json`](performance/browser-parity-m2.json).
Browser entries are selected summaries; native final entries retain full reports.
The exploratory `baseline`/`candidate` entries predate queue draining and are
excluded from the completed-FPS table. Version 2 `final`/`pre-bundle` entries
describe the retained direct-draw implementation before the bundle experiment.

## Changes retained

- Browser rendering maintains one pending animation-frame callback. Gamepads
  are polled immediately before input processing, without a second continuous
  event-loop polling task or replacement of a pending callback on ordinary input.
- GPU profiling is created and read back only while F3 is visible. Normal play
  avoids timestamp resolve, copy and mapping work. F3 distinguishes supported
  timestamps from active profiling.
- F3 shows effective physical framebuffer and world dimensions, display scale,
  MSAA, render scale, grass draw calls and quality. Settings that require a
  restart/reload are distinguished from the configuration currently rendering.
- GPU metadata comes from the device configured on the actual canvas. Here the
  browser exposes `apple / metal-3`, explicitly reports a non-fallback adapter,
  and withholds the exact model. Native reports `Apple M2`. Missing fields stay
  unavailable; the application does not infer an exact GPU from a vendor string.
- A common native/browser benchmark makes future comparisons repeatable and
  rejects GPU/device failures instead of reporting invalid throughput.

The scheduling change removes redundant work, but the exploratory callback
timings did not establish a consistent FPS improvement on this GPU. No grass
density, shader quality, shadow, bloom, MSAA or render-scale reductions are hidden
in these changes.

## Experiments rejected

Cached WebGPU render bundles replayed persistent indirect grass draws, with
unchanged root order and LOD counts. All 13 native capture comparisons were
byte-identical, including day/night/race/victory at 1×/2×/4× MSAA and a culled/LOD
view. But two 1440p stationary browser runs returned 52.8 and 52.6 FPS, versus
52.9 FPS in the interleaved direct-draw control. Encoding times were also similar.
The prototype was removed: it did not demonstrate a benefit on the tested GPU.

Contiguous grass-instance coalescing reduced the stationary scene from 225 draws
to 225 and the race from 201 to 200. It did not remove enough work to justify the
extra implementation and was removed.

Disabling grass alpha-to-coverage looked equivalent in a daylight fixture but
changed night/race MSAA output. A repeated same-setting capture was stable;
the 4× race comparison changed 51,789 RGB bytes, approximately 2.2% of channels.
The original alpha-to-coverage setting is retained.

## Reproduce

```sh
cargo web bench
cargo run --release -p lawn_orbit -- --benchmark
cargo run --release -p lawn_orbit -- --benchmark --scene=race --gpu-timing
cargo run --release -p lawn_orbit -- --benchmark --resolution=1440p
cargo run --release -p lawn_orbit -- --benchmark --scene=race --resolution=1440p
```

The browser page at `/benchmark.html` offers the same workloads and optional GPU
timing. Run one GPU workload at a time, keep its window visible and close other
games/build jobs. Compare profiler-off runs for normal throughput, then use
separate profiler-on runs to locate GPU cost. Both targets print JSON and stop
rendering when finished. Native `cpu_frame_including_present_ms` includes a
presentation wait and is not directly comparable to browser CPU encoding time.

Cross-platform world hashes differ; the measured workload counts agree and
final mowing coverage differs only in very small floating-point terms. A hash
mismatch alone does not establish the magnitude of geometry differences, and
this is not a claim of bitwise cross-platform simulation identity. Acceptance
so far covers this M2 and in-app browser only;
Firefox, Safari, discrete GPUs, mobile devices and high-refresh displays need
their own measurements. Full-game HUD, editor generation, loading and memory
usage should be profiled separately from this renderer benchmark.

## Validation

The retained implementation passes 236 ordinary workspace tests, strict native
and WASM Clippy, formatting checks, and native/WASM release builds. Three
additional native GPU smoke tests cover ordinary play, racing and victory at
the supported 1×/2×/4× sample counts. The rejected bundle experiment's 13 exact
image comparisons are recorded alongside the measurement data.
The final browser release also passed title/editor/race startup, F3 open/close,
and pause checks with no console warnings or errors. The validated game was left
paused on the local development server.
