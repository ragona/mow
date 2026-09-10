# Performance investigation

## CPU preparation and mowing

Measured on Apple M2 with the release build, generator version 2, seed `42`,
512×512 mowing cells per face, and 409,320 generated grass roots. The driving
workload runs 3,600 simulation ticks (30 seconds at 120 Hz), extracting dirty
uploads every second tick. Timings are local observations, not performance
assertions or whole-frame GPU measurements.

```sh
cargo run --release -p lawn_tools --example performance
cargo run --release -p lawn_tools --example performance -- --owned-uploads
```

The benchmark now reports generation with and without roots, mowing and physics
preparation separately, simulation percentiles, tile extraction time/count/bytes,
and cold versus cached locator time. `--owned-uploads` exercises the retained
allocating compatibility API; the default uses the renderer's borrowed visitor.

| Finding, in priority order | Before | After | Change |
| --- | ---: | ---: | --- |
| Gameplay preparation | 74.53–75.11 ms | 37.49 ms | Cache exact cube-face solid angles and shared corner terms |
| Dirty tile extraction over the workload | 4.27–4.31 ms | 1.32 ms | Reuse one packed-cell staging buffer; copy contiguous rows |
| Median dirty extraction | 2.50–2.54 µs | 0.75 µs | Same borrowed upload path |
| Owned upload vectors allocated | 25,340 | 0 | Retain dirty keys and staging storage |
| Simulation including extraction | 112.19–112.93 ms total | 108.84 ms total | Upload extraction savings; simulation unchanged |
| Generation including roots | 58.49–59.62 ms | 58.71 ms | No generator changes warranted |

The before ranges are three warmed runs; after values are the first clean run
following the changes. Later concurrent profiling raised generation and tick
times too, so those runs are excluded from the speed comparison. The allocation
counts cover explicitly owned upload vectors, not every allocator call inside
Rapier, the graphics driver, or the application.

Mowing preparation was the largest measured CPU startup cost at roughly 72 ms;
physics preparation took 3.3–3.9 ms. Exact solid-angle evaluation repeated the
same transcendental calculations on six faces and four corners per cell. The
cache keeps the original floating-point expression and accumulation order. It
adds about 2 MiB of temporary memory at shipping resolution, released after
preparation, and scales to 32 MiB at the maximum supported 2,048 resolution.

Dirty uploads are now delivered through `MowingField::visit_dirty_tiles` with a
borrowed `DirtyTileView`. Queue writes copy the data before the callback returns,
allowing every tile to reuse a 1 KiB staging buffer. Sorted tile order, partial
edge tiles, and `take_dirty_tiles` compatibility are preserved. Removing this
allocation churn is a small runtime improvement; the simulation already costs
only about 0.03 ms per tick.

The application review found profile saves only on explicit actions or tutorial
completion, with no per-frame profile I/O or lock bottleneck. Locator caching
already reduces unchanged queries to timer resolution; the cold query costs
about 2.2 ms. Neither path justified a redesign.

Validation preserves the driven coverage exactly (`0.15317433724644383`) and
the upload output (23,904 tiles, 24,477,696 bytes). Tests compare cached solid
angles bit-for-bit against the previous expression at five resolutions,
including 512. Borrowed upload tests reconstruct every packed cell across six
faces at resolutions 8, 19, and 64; they check partial tiles, sorting,
deduplication, repeated drains, and stable staging address/capacity. Core tests
and strict core/tools Clippy pass.

## Sky preparation and compositing

Sky generation previously spent roughly 206 ms evaluating cloud noise and
135 ms encoding linear colors to sRGB. A monotonic lookup of exact floating-point
quantization boundaries reduces the encoding phase to about 35 ms while retaining
every output byte. In-place mip compaction also reduces filtering from about
10 ms to 5 ms and avoids large temporary buffers. The complete bake improves
from a warmed median of 370 ms to 268 ms on the M2. Resolution, stars, noise,
colors, and the entire 33,554,424-byte mip chain are unchanged (checksum
`a5a919d46f50ea6a`). Dense-input and rounding-boundary tests compare the lookup
against the original `powf` encoder.

The composite shader now projects the three fullscreen vertices into homogeneous
world coordinates and interpolates them. Perspective division and normalization
remain per pixel. This removes a matrix multiply per pixel while preserving
camera translation and shifted editor projection. GPU comparisons against the
previous shader covered 294,912 sky pixels across gamma and sRGB framebuffers;
98 color channels differed by one 8-bit level, with all others identical.

## Measurement corrections

CPU encoding used to include the wait for a presentation image, which made a
Vsync wait appear to be rendering work. Diagnostics now report acquisition and
encoding separately. GPU timestamps also now include the entire frame span,
from the beginning of interaction compute through the end of compositing.
Individual pass timestamp windows overlap on this Metal device; summing them
would overstate the GPU cost. The total excludes UI drawing and presentation.
Timestamp readback remains asynchronous and skips samples when its ring is busy.

## Grass visibility

The dominant GPU workload was grass: the Craggy camera submitted 335,573 tufts,
with 18 triangles per tuft. The old visibility test combined the tallest point
anywhere on the planet with a loose forward-angle cutoff. This kept many hidden
patches in the draw list.

The renderer now caches the actual root AABB, enclosing sphere, and maximum
radius of each patch. Every frame it expands those bounds for the full blade
length, width, and maximum bending, then tests the actual projection's six
frustum planes and the patch's local horizon. The horizon uses an inner sphere
beneath the minimum rendered terrain radius, with a conservative correction for
the cube mesh's triangle span. This covers crater depressions and avoids the
excessively small bound obtained from infinitely extended mountain-face planes.
The cache costs 32 bytes per patch, is built once per planet, and needs no
per-frame allocations. Root order, blade geometry, effects, and density/LOD
prefixes are unchanged.

In the Craggy workload, submitted tufts fell to 279,060. A reference draw with
visibility rejection disabled produced exactly the same 921,600 RGB pixels.
Compared with the original renderer, only 200 color channels differed by one
8-bit level, from the composite projection change. CPU regression tests cover
shifted editor projection, WebGPU near/far planes, blades bending into view,
elevated horizon blades, and exact density retention. Independent triangle
closest-point checks verify that the occluder stays inside real cratered meshes
at several terrain resolutions.

## Experiments not retained

- A compute pass prepared shared grass state once per root, replacing repeated
  vertex work. GPU readback confirmed equivalent vertices to roughly 1e-6, but
  after visibility improvements its frame median was 11.24 ms versus 11.31 ms
  for the direct path, less than a 1% difference. It also required an additional
  42.7 MB buffer in Craggy. The implementation was removed; the direct shader
  remains unchanged.
- Compacting roots already collapsed by the grass/rock contour would remove
  only 1,095 of 533,714 Craggy roots (0.205%) and 344 of 409,320 shipping roots
  (0.084%). Meadow has none. These already exit early in the vertex shader.
  The tiny memory saving did not warrant new indexing to preserve LOD prefixes;
  compaction was inspected and counted but not implemented or GPU-benchmarked.
- Bloom already uses quarter-resolution targets, paired blur taps, and a Low
  quality bypass. No reduction in resolution, grass density, MSAA, texture
  detail, or lighting was made for the reported improvements.
