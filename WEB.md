# Browser target

Lawn Orbit runs locally in the browser as Rust WebAssembly plus core WebGPU.
The native and browser targets share gameplay, Rapier physics, the world editor,
egui interface, WGSL shaders, grass density, shadows, HDR, bloom, and MSAA controls.
This target does not use server rendering or a separate game implementation.

**Development.** Install Rust 1.93+, `wasm32-unknown-unknown`, and the
`wasm-bindgen-cli` version selected in `Cargo.lock` (currently 0.2.127). Run
`cargo web` from this workspace to build, serve, and open the game. It defaults to
an optimized release build because grass rendering and preparation should be
tested at realistic performance. The initial build takes longer than incremental
builds. `cargo web --no-open --port 8088` is useful when a test tab is already open.
Stop with Ctrl+C, rerun after changes, and reload; this command does not watch
files or hot-reload Rust state. Development responses disable caching.

The CLI checks tool versions and prints the exact setup command when a
prerequisite is missing. All generated output stays under `target/`. It clears
`target/web` before packaging, so obsolete JS snippets cannot survive a rebuild.
The server binds only to loopback. It serves WASM as `application/wasm` and ES
modules as JavaScript.

**Runtime.** `web/main.js` initializes the module and calls its explicit `start`
export. The Rust app awaits initial world and sky preparation, then creates its
canvas surface and awaits adapter/device initialization before starting winit's
browser event loop. Native initialization retains its desktop entry point.

`web/worker.js` initializes the same module without starting the game. Requests
and responses have a versioned, bounded binary format. Each job transfers its
result buffer and terminates the worker when consumed or canceled. Initial
preparation produces both the world and the full sky mip chain; editor jobs
prepare worlds. Errors, module-load failures, invalid responses, and timeouts
reach the loading screen or editor. Workers own separate WASM memories and need
no shared-memory thread toolchain or cross-origin isolation headers.

Prepared worlds include the mowing arrays and weights, so returning from a
worker does not repeat the expensive mowing setup on the rendering thread.
Small physics/runtime objects are reconstructed there. GPU resources are created
on the rendering thread. Sky preparation preserves every texel and mip level;
there is no low-resolution browser substitution. Transferring data between
independent WASM memories still entails temporary copies, so repeated world
changes deserve memory profiling.

Browser clocks use `web_time`; the normal Rust filesystem and native thread APIs
are excluded from browser platform paths. Profile RON is stored under
`lawn-orbit.profile.ron` in localStorage. Loading an invalid/future profile starts
a session without writes and surfaces a message. Save attempts also validate
existing data before replacement. Storage refusal does not prevent playing.
Profiles are origin-specific: changing hostname or port uses a different profile.

Continuous browser rendering uses one pending `requestAnimationFrame` through
winit's `Wait` event loop. Browser gamepads are polled immediately before each
frame's input processing. Ordinary input events do not cancel and replace the
pending frame, and there is no extra continuous polling task between frames.
Native event-loop scheduling is unchanged.

Keyboard/mouse and browser gamepad input use the existing controls. The canvas
owns game keys and right-drag while browser copy/paste events bridge to egui.
Fullscreen is requested through a user action, not automatically on page load.
Browsers own Escape/fullscreen behavior and may reserve some shortcuts. Rumble
is unavailable through the current gilrs browser backend. Touch controls and
WebGL2 fallback are outside this target. Hidden/unfocused play pauses and does
not attempt to catch up an entire background interval on return.

**Checks.** Native and WASM CI are both in `.github/workflows/ci.yml`. The browser
job runs strict target-specific Clippy, packages the actual release bundle, and
uploads it as a workflow artifact. Native CI runs formatting, strict Clippy and
the complete ordinary workspace suite. Locally:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --target wasm32-unknown-unknown -p lawn_orbit --lib -- -D warnings
cargo web build
cargo web
```

Worker tests round-trip generated terrain, roots, and precomputed mowing through
the binary protocol, then compare a Turf Race against direct native preparation.
They also reject malformed/version-mismatched messages. Sky tests compare the
serial browser computation with native face batching and validate mip dimensions.
Profile and input tests retain their native regression coverage. These checks
complement real browser testing; compiling WASM alone does not exercise WebGPU.

Local acceptance on September 10, 2026 used an Apple M2 and the Codex in-app
browser with the `BrowserWebGpu` backend. Both Turf Race and Free Mow rendered
and ran; worker-generated seed/preset changes, keyboard driving, pause, settings
persistence across reloads, and seed copy/paste were exercised. The final release
bundle starts successfully without browser warnings or errors. Native validation
passed 228 ordinary tests and three additional GPU smoke tests, plus native and
WASM strict Clippy and release builds. The subsequent controlled comparison is in
`BROWSER_PERFORMANCE.md`. Firefox, Safari, and physical gamepads still require
separate acceptance work.

For browser acceptance, use both Turf Race and Free Mow, change world presets and
seeds, enter the exact preview, drive/cut/boost, pause and switch tabs, restart,
and inspect results/victory behavior. Reload after changing settings/favorites,
verify seed clipboard input, resize, and test fullscreen. Watch F3 diagnostics and
the browser console for rendering and preparation errors. Preserve an unreadable
or future-version localStorage profile when testing session-only behavior.

**Performance.** Browser builds preserve the same settings rather than silently
lowering quality. Compare native/browser on the same GPU, seed, camera and physical
framebuffer size. Device pixel ratio affects that size: a 1280×720 CSS canvas on
a 2× display renders many more pixels than a 1280×720 framebuffer. Use render
scale, MSAA, and grass density explicitly when tuning. Standard and High both
retain full density and bloom; Low reduces density and disables bloom.

`PERFORMANCE.md` contains native measurements, not guaranteed browser frame rates.
F3 GPU timestamps are optional and depend on the browser's advertised features.
They are collected only while F3 is visible; normal play does not resolve or map
timestamp buffers. F3 distinguishes supported timestamps from active profiling.
It shows effective physical framebuffer/world sizes, MSAA, render scale, and
quality, including when settings have pending changes that require a reload.
GPU identity comes from the configured canvas's actual device; missing fields
remain unavailable. A redacted description falls back to the exposed vendor and
architecture, without guessing an exact model or inferring software rendering.
Warm pipelines before comparing frame pacing, include active races, and measure
loading/editor stalls independently. Cross-platform floating-point bit equality
is not implied by same-platform deterministic tests.

Use the shared benchmark to compare fixed workloads:

```sh
cargo web bench
cargo run --release -p lawn_orbit -- --benchmark --gpu-timing
cargo run --release -p lawn_orbit -- --benchmark --scene=race --resolution=1440p --gpu-timing
```

The browser benchmark is also available at `/benchmark.html` on an existing
development server. Its links select stationary/race scenes, optional GPU timing,
and 720p/1440p physical render targets. Both targets use seed 42, the same Craggy
configuration, High quality, 4× MSAA, full render scale, 90 warmup frames and 360
measured frames. The race advances one fixed simulation tick per submitted frame;
particle, interaction and shader animation clocks use that same fixed delta.
That is one 1/120-second tick per frame, less simulation work than a live 60 FPS
game's usual two ticks; this isolates a matched renderer/script workload.
No profile is loaded or saved, and the renderer runs without egui or DOM game UI.

Version 2 drains GPU work after warmup and again after measurement. Its completed
throughput includes CPU work, submission, pacing and callback delivery, while
`frame_ms` records CPU redraw intervals. Neither measures compositor display
flips. This distinction matters when CPU callbacks run faster than the GPU can
finish. GPU spans are asynchronous latest samples, not unique per-frame samples.
Close other GPU workloads and keep the benchmark visible. The native benchmark
prints JSON and exits; the browser prints the same report on its page and stops
rendering. See `BROWSER_PERFORMANCE.md` for local results and comparison limits.

**Deployment.** Run `cargo web build` and serve the complete `target/web` directory
at an HTTPS static origin. Preserve relative JS/WASM/worker paths, JavaScript and
WASM MIME types, and deploy assets together so worker/main module versions match.
Localhost is a secure development context. A production host may compress and
cache versioned bundles; avoid caching a stale loader against new WASM. No
application backend or account is required.

GitHub Pages publishes this repository from `main` using `.github/workflows/ci.yml`.
In repository **Settings → Pages → Build and deployment**, select **GitHub Actions**
as the source. Each push to `main` (or manual CI run on `main`) builds the WASM
distribution and deploys it only after the native and browser checks pass.
Pull requests and other branches run checks without publishing. Deployment
permissions are limited to the deployment job, and a new push does not cancel
an in-flight main deployment. Relative module and worker paths support the
project URL at `https://ragona.github.io/mow/`.

Browser availability depends on browser version, OS, GPU, and graphics
acceleration. Require successful core WebGPU adapter/device creation, not merely
the presence of `navigator.gpu`. See the
[GPUWeb implementation status](https://github.com/gpuweb/gpuweb/wiki/Implementation-Status)
and [wgpu web build documentation](https://wgpu.rs/doc/wgpu/documentation/platforms/web/index.html).
