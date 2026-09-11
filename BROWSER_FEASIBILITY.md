# Lawn Orbit browser feasibility report

Implementation follow-up: the browser target described by this assessment is now
available through `cargo web`. See [WEB.md](WEB.md) for architecture, development
commands, and validation results. The assessment below preserves the original
baseline; the subsequent matched-resolution measurements are documented in
[BROWSER_PERFORMANCE.md](BROWSER_PERFORMANCE.md).

Assessed September 10, 2026 against commit `2b85757` (`victory lap`), Rust 1.93.1, and the checked-in dependency lockfile. The lockfile resolves `wgpu` 29.0.4, `winit` 0.30.13, and egui 0.35.0. Scope: running the existing game locally in a desktop browser, with its current gameplay and graphics. This is an engineering assessment, not an implemented browser port or a browser performance measurement.

**Recommendation: proceed with Rust → WebAssembly plus WebGPU. Full visual fidelity is highly feasible; matching native frame rate is plausible on suitable hardware, but remains unproven.** The code already has the right graphics abstraction and separates simulation from rendering. There is no identified need to change engines or rewrite the game in JavaScript. The work is principally browser lifecycle, platform services, loading, and validation.

For this report, “full performance” means the same foreground frame rate on the same GPU, at the same physical framebuffer resolution, scene, grass density, effects, and MSAA. Lowering resolution or density is a useful fallback, but would not establish parity. Mobile devices, every browser/GPU combination, and unrestricted background execution are separate targets.

| Route | Fit for this repository | Assessment |
| --- | --- | --- |
| Rust WASM + core WebGPU | Retains `lawn_core`, `lawn_render`, WGSL, Rapier, and egui | Recommended; smallest credible route to the existing experience |
| WASM + WebGL2 | Grass interaction needs compute, and grass rendering reads vertex-stage storage buffers | Substantial rendering adaptation; not a configuration-only fallback |
| JavaScript/TypeScript engine rewrite | Reimplements working rendering, simulation, and UI | No demonstrated performance benefit to justify the scope |
| Emscripten or WASI wrapper | Does not remove canvas, event-loop, storage, and GPU integration work | Poorer fit than the existing Rust/browser ecosystem |
| Stream a native executable | Native rendering happens on a server | Different latency, hosting cost, and offline properties; does not run the game locally in-browser |

wgpu explicitly supports browser WebGPU and passes WGSL through to the browser. The expensive grass shader work remains GPU work; WASM does not simulate millions of grass vertices on the CPU. WebGL2 lacks the compute support used here. [wgpu browser support](https://docs.rs/crate/wgpu/29.0.1), [wgpu downlevel capabilities](https://docs.rs/wgpu/latest/wasm32-unknown-unknown/wgpu/struct.DownlevelFlags.html).

**Build evidence separates dependency fixes from runtime porting.** The unmodified command below failed in `getrandom` 0.3.4 because its browser entropy backend is not configured:

```sh
cargo check --locked --target wasm32-unknown-unknown -p lawn_orbit
```

The dependency tree shows that `rand`'s default OS/thread RNG features introduce this requirement. Generation actually uses explicitly seeded `ChaCha8Rng`; it does not need OS entropy. In a disposable copy, disabling `rand` defaults while retaining `std` passed that failure. The next check failed in `arboard` 3.6.1, pulled in by `egui-winit`'s default clipboard feature. Disabling those defaults exposed one application type error: winit's browser `ControlFlow::WaitUntil` expects `web_time::Instant`, while the app supplies `std::time::Instant`.

After also changing the app's `Instant` import to `web_time::Instant`, **`cargo check` passed for `lawn_core`, `lawn_render`, and `lawn_orbit` on `wasm32-unknown-unknown`**. No core or renderer source changes were needed for this type-check. This is not a linked release build, browser execution, or proof that the remaining standard-library calls work. All diagnostic edits were confined to a disposable copy; this repository's application source and manifests remain unchanged.

The two diagnostic manifest changes were:

```toml
rand = { version = "0.9.2", default-features = false, features = ["std"] }
egui-winit = { version = "0.35.0", default-features = false }
```

A production patch should preserve desired native egui integration with target-specific dependency features. Compile success alone cannot validate browser runtime behavior: this Rust target permits some standard-library APIs that fail when called. [Rust target limitations](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html).

**The renderer fits core WebGPU's capabilities.** All shipping shaders are WGSL. The renderer enables optional GPU features only when advertised; its native-only texture-format feature is therefore not a mandatory dependency. The “enhanced” indirect-draw tier is diagnostic and does not gate the actual grass draw path. [Device setup](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:236).

| Existing workload | Size or requirement | Core WebGPU fit |
| --- | --- | --- |
| Grass interaction compute | Five storage bindings; 64 invocations/workgroup; 1,536 workgroups | Within core defaults of eight storage bindings, 256 invocations, and 65,535 groups per dimension |
| Interaction fields | Four 1.5 MiB storage buffers | Well below the default 128 MiB storage binding limit |
| Textures | 2048² shadows, 1024²×6 sky, 512²×6 mowing | Within core texture dimensions and array-layer limits |
| Lighting and effects | RGBA16F HDR, Depth32Float, 4× MSAA, shadows, bloom | Existing capability selection supports this without mandatory native extensions |
| Grass geometry | Instanced vertex data plus vertex-stage storage read | Supported by core WebGPU; not guaranteed by compatibility mode |

Sources: [interaction implementation](/Users/ragona/code/mower/crates/lawn_render/src/interaction.rs:47), [MSAA selection](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:1186), [grass bindings](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:1448), [WebGPU limits and format requirements](https://gpuweb.github.io/gpuweb/#limits).

Target **core WebGPU** first. Do not advertise compatibility-mode-only adapters or WebGL2 as equivalent support. Actual browser shader compilation and rendering still need validation even when resource limits fit.

**The platform changes are concrete and bounded.**

| Area | Current behavior | Browser work |
| --- | --- | --- |
| Startup | Desktop `main`, window setup, blocking GPU initialization | Add a WASM entry, HTML canvas attachment, console/panic reporting, and asynchronous renderer-ready/error states. Use winit's browser event loop. |
| Clocks | `std::time::Instant` in app, input, renderer, particles, sky; `SystemTime` for seeds | Use `web_time` clock types. The workspace already declares this dependency. `Duration` itself is fine. |
| World preparation | Native thread moves a completed `RunState` through an mpsc channel | Browser Worker or incremental preparation; merely marking generation `async` does not prevent a long blocking task. |
| Sky preparation | Scoped native threads, including when worker count falls back to one | Execute a true serial loop for the first spike; preferably pre-bake the immutable sky for production. |
| Profiles | App-data directory, filesystem reads, flushes, atomic rename | Storage adapter using the same RON schema. Small profiles can use localStorage; IndexedDB supports asynchronous storage. Preserve protection against overwriting unreadable/future-version data. |
| UI integration | egui/winit desktop clipboard defaults | Keep egui and its renderer; disable native clipboard dependencies on WASM and bridge browser copy/paste for seeds. |
| Input and lifecycle | Desktop focus, fullscreen, mouse and gamepad behavior | Verify canvas focus, scrolling/default keys, right-drag/context menu, resizing, fullscreen user gestures, and hidden-tab pause/resume. |
| Gamepad feedback | gilrs input and force feedback | gilrs has a browser Gamepad backend; it is not inherently a port blocker. Its current WASM backend does not provide rumble. A browser vibration adapter would be separate work. |
| Tests | Native GPU tests block on completion/readback | Add an asynchronous browser harness using the same scene fixtures and image checks. |

Relevant implementation: [entry point](/Users/ragona/code/mower/crates/lawn_orbit/src/main.rs:8), [blocking GPU setup](/Users/ragona/code/mower/crates/lawn_orbit/src/app.rs:396), [world worker](/Users/ragona/code/mower/crates/lawn_orbit/src/app.rs:774), [sky worker loop](/Users/ragona/code/mower/crates/lawn_render/src/sky.rs:220), [profile storage](/Users/ragona/code/mower/crates/lawn_orbit/src/profile_store.rs:33), [input adapter](/Users/ragona/code/mower/crates/lawn_orbit/src/input_adapter.rs:57). Browser support: [winit web event loop](https://docs.rs/winit/0.30.13/winit/platform/web/trait.EventLoopExtWebSys.html), [web-time](https://docs.rs/web-time/1.1.0/web_time/).

Keep simulation and rendering together on the main thread initially. Offload world preparation if measurements confirm editor/loading stalls, which the native startup costs make likely. `Planet` and `RunState` are not currently serializable: the native worker's complete-state handoff needs an explicit browser representation. Transfer flat prepared arrays and reconstruct small runtime objects; returning only a planet would leave full mowing preparation on the main thread. Budget for copying data into a separate WASM memory. [RunState construction](/Users/ragona/code/mower/crates/lawn_core/src/run.rs:138).

Shared-memory WASM threads are another option, but the steady-state simulation does not presently justify that build complexity. Ordinary single-threaded WASM and message-based Workers do not require shared memory. Shared-memory threads add cross-origin isolation and, with common Rust tooling, atomics and a rebuilt standard library. Moving the entire game into an OffscreenCanvas Worker also introduces input/DOM bridges; the current gilrs backend expects a browser window. [wasm-bindgen-rayon setup](https://github.com/RReverser/wasm-bindgen-rayon), [worker GPU access](https://developer.mozilla.org/en-US/docs/Web/API/WorkerNavigator/gpu), [shared-memory restrictions](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer).

**GPU headroom, rather than basic WASM compatibility, is the main performance risk.** The latest checked-in visual-polish benchmark uses Apple M2, Craggy, 1280×720, High quality, 4× MSAA, 60 warmup frames and 120 samples:

| Native measurement | Recorded result | Implication |
| --- | ---: | --- |
| GPU frame median / p95 | 14.519 / 15.371 ms | About 2.15 / 1.30 ms below a 16.67 ms budget |
| World-pass median | 13.872 ms | Grass/world rendering dominates |
| CPU encoding median | 0.172 ms | Native command encoding is inexpensive |
| Visible grass | 279,060 tufts, 18 triangles each | About 5.02 million grass triangles/frame |
| Free Mow simulation | About 0.03 ms/tick | Encouraging CPU margin, but does not benchmark race AI and rival physics |

These are existing native observations, not measurements made by this report. GPU totals exclude UI and presentation; individual pass spans overlap and must not be added. The CPU workload is explicitly Free Mow. Refresh the native baseline at the ported revision and measure Turf Race separately. [Recorded benchmarks](/Users/ragona/code/mower/PERFORMANCE.md:136), [CPU workload](/Users/ragona/code/mower/crates/lawn_tools/examples/performance.rs:34).

The GPU workload remains similar in-browser, but browser shader translation, validation, scheduling, and compositing can change its cost. There is insufficient evidence for a universal “90–100% of native” claim. Even native High quality has little measured margin on the reference M2, and 120 FPS at those settings is already outside its recorded GPU budget.

The renderer issues one instanced draw per visible grass patch, with at most 384 patches at shipping terrain settings. It does not cross into browser APIs once per tuft. Profile those draw calls and dirty-tile texture uploads before investing in batching or render bundles. Keep simulation/state in WASM rather than serializing it to JavaScript every frame. Try `simd128` as a measured CPU build variant; it will not remove the GPU grass workload. [Draw loop](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:964), [dirty uploads](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:685).

Existing fallbacks are available: Low halves grass density and skips bloom, render scale can fall to 0.5, and MSAA is separately selectable. Standard and High both retain full density and bloom; switching between those presets alone does not reduce these rendering costs. Use reductions honestly as quality settings. A 2× device pixel ratio creates four times as many framebuffer pixels if followed directly; benchmark physical canvas dimensions, not CSS dimensions. Do not revive the previously rejected per-root compute cache without fresh evidence: its recorded gain was below 1% while adding 42.7 MB. [Quality settings](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:1213), [rejected experiment](/Users/ragona/code/mower/PERFORMANCE.md:121).

**Startup and memory need their own budget.** Native generation with roots is approximately 59 ms, gameplay preparation approximately 37 ms, and the multithreaded sky bake approximately 268 ms in the recorded investigations. These are different preparation measurements, not an additive browser loading-time prediction. Renderer preparation adds further work. [Preparation measurements](/Users/ragona/code/mower/PERFORMANCE.md:1).

Derived allocation sizes identify where to investigate; they are not measured process memory:

- The sky bake retains a 72 MiB floating-point image while accumulating roughly 32 MiB of encoded mipmaps. An exact pre-baked sky asset can remove that computation and temporary allocation while preserving appearance. Measure compressed download size; embedding the raw mip chain would add about 32 MiB to the payload.
- Selected persistent GPU resources total roughly **122 MiB** in the 720p/4× Craggy setup: about 49.2 MiB HDR/depth targets, 16 MiB shadows, 32 MiB sky, 0.9 MiB bloom, 6 MiB interaction, 6 MiB mowing, and 12.2 MiB roots. Terrain, particles, UI, swapchain, and browser overhead are additional.
- At 512-face resolution, the main mowing arrays occupy about 19.5 MiB in Free Mow and 21 MiB in Turf Race, excluding ancillary allocations. Simultaneous old/new previews and Worker transfers raise peak memory.
- Custom maxima need independent guards. Mowing resolution 2048 multiplies its main arrays by 16. The 16-million-root allocation ceiling could permit a 384-million-byte render buffer, exceeding the core default 256 MiB `maxBufferSize`. Validate requested worlds against the device and memory budget rather than treating configuration validity as GPU compatibility.

References: [sky allocation](/Users/ragona/code/mower/crates/lawn_render/src/sky.rs:215), [render targets](/Users/ragona/code/mower/crates/lawn_render/src/renderer.rs:1686), [mowing arrays](/Users/ragona/code/mower/crates/lawn_core/src/mowing.rs:113), [root ceiling](/Users/ragona/code/mower/crates/lawn_core/src/planet.rs:611), [render root layout](/Users/ragona/code/mower/crates/lawn_render/src/grass_roots.rs:10).

**Deployment can be a static site, with explicit browser support.** Use `wasm32-unknown-unknown`, `wasm-bindgen`, a small HTML/JS loader, release optimization, compressed delivery, and cacheable versioned assets. Trunk is an optional build/serve wrapper. Match the wasm-bindgen CLI to the resolved library version. Serve WASM with `application/wasm` and deploy over HTTPS; localhost is suitable for development. Ordinary WebGPU does not inherently require COOP/COEP headers. [wgpu web build guide](https://wgpu.rs/doc/wgpu/documentation/platforms/web/index.html), [Trunk](https://github.com/trunk-rs/trunk).

The working group's implementation status, updated August 13, 2026, reports:

| Browser | Practical starting support |
| --- | --- |
| Chrome / Edge | Windows x86/x64, macOS, ChromeOS; Windows ARM64 still requires a flag |
| Chromium on Linux | Selected configurations: Intel Gen12+ from 144; NVIDIA on Wayland with driver 535.183.01+ from 147; other setups remain restricted |
| Firefox | Windows from 141; Apple Silicon macOS from 147; Intel macOS and Linux remain Nightly-only |
| Safari | Safari 26 on macOS Tahoe 26; also ships on iOS/iPadOS/visionOS 26, without implying this game's mobile performance |

Check `navigator.gpu`, adapter creation, device limits, and actual shader/pipeline creation; browser branding alone is insufficient. [GPUWeb implementation status](https://github.com/gpuweb/gpuweb/wiki/Implementation-Status). Background animation scheduling differs from desktop execution, so preserve pause/resume semantics. [Animation frame behavior](https://developer.mozilla.org/en-US/docs/Web/API/Window/requestAnimationFrame).

**A staged implementation should answer the expensive uncertainty early.** These are planning estimates for one engineer familiar with Rust/wgpu, not delivery commitments.

| Stage | Deliverable | Estimated effort |
| --- | --- | ---: |
| Performance spike | Fix dependency features, clocks, canvas/async startup; eliminate native-thread calls; render and play a fixed world with all effects; collect initial native/browser comparisons | 2–4 engineer-days |
| Complete browser experience | Responsive world editor/preparation, sky asset strategy, browser saves, seed clipboard, input/fullscreen/focus behavior, loading/error UI | 5–8 additional days |
| Validation and measured tuning | Browser matrix, Turf Race and worst scenes, image/gameplay checks, memory/loading analysis, regression build and deployment packaging | 3–6 additional days |
| Total initial scope | Usable desktop-browser release candidate | Approximately 10–18 engineer-days |

Cross-browser driver defects, strict cross-platform deterministic hashes, mobile support, WebGL2 fallback, and substantial shader optimization can extend that scope. No estimate here guarantees native frame-rate parity.

For the spike, compare the same machine/GPU, physical resolution, seed, camera, quality, and MSAA. Cover Meadow, Craggy, heavily mown grass, and Turf Race. Start with 720p High/4× to match the recorded reference, then test 1080p and realistic high-DPI configurations. Use a release build, warm shader pipelines, and run several minutes of scripted foreground play after warmup. Record actual animation-frame intervals and missed refreshes, CPU simulation/encoding, optional asynchronous GPU timestamps, startup time, and peak allocations. FPS alone hides stalls; an offscreen GPU number does not establish end-to-end frame pacing.

As a proposed decision gate, require complete rendering/gameplay and no recurring stalls first. On target hardware where native sustains 60 Hz, require similarly stable browser pacing at identical settings, and agree on an acceptable native/browser cost delta before calling it parity. A 10% median/p95 cost delta is a useful investigation threshold, not a forecast. Keep image comparisons tolerant of small backend differences while checking grass density, effects, interaction, and mowing results explicitly.

Release validation should exercise both game modes, editing and entering the previewed world, race results and victory laps, restarting, remapping, profile reload, and pause/resume after switching tabs. Repeated world changes must not accumulate memory. Retain native tests and add the browser target to build checks.

Verify generated seed fixtures and gameplay across native and WASM: the current planet hash includes raw floating-point bits, so same-platform determinism does not prove identical cross-platform hashes. This matters for sharing exact worlds, even without multiplayer. [Planet hashing](/Users/ragona/code/mower/crates/lawn_core/src/planet.rs:1058).

The recommended next investment is the 2–4 day browser spike. It should establish how much frame-time margin the browser consumes before broader packaging or compatibility work. The evidence supports preserving the game's graphics and architecture; the unresolved question is sustained performance at a precisely defined hardware and quality target.
