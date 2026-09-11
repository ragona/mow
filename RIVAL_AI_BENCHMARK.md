# Rival AI benchmark

Verified on September 11, 2026, using the final local implementation and the headless harness in `crates/lawn_core/tests/rival_competition.rs`.

## Competitive results

Each match runs until a mower owns more than half the grass or 180 simulation seconds elapse. Every world is tested with both complete vehicle spawn assignments swapped. The table reports **match counts**, and the average final ownership difference in **percentage points**.

| Cohort | Opponent | Mowing resolution per face | Rival wins | Losses | Unfinished | Mean final lead |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Original worlds | Greedy | 128 | 8 / 8 | 0 | 0 | +2.81 pp |
| Original worlds | Frozen original AI | 128 | 8 / 8 | 0 | 0 | +29.59 pp |
| Additional rocky seeds | Greedy | 128 | 10 / 12 | 2 | 0 | +2.72 pp |
| Shipping resolution | Greedy | 512 | 4 / 4 | 0 | 0 | +3.30 pp |

There were no draws or vehicle recoveries in these 32 final matches. The two losses were the original spawn assignment on additional seeds 3 and 99: the rival finished at 47.79% and 47.66%, respectively, against approximately 50%.

The original AI, evaluated from commit `e89bdb1ac46c65e47200c7fc351999993bb42d04` with the same greedy harness, lost all eight original-world matches. Its average final ownership deficit was 27.92 percentage points. The copied original policy is retained in `crates/lawn_core/tests/support/legacy_rival.rs` for direct comparisons.

- **Original worlds:** one featureless flat sphere plus rocky seeds 7, 17, and 55; two spawn assignments each. Flat worlds are identical when all terrain variation is disabled, so duplicate flat seeds are skipped.
- **Additional rocky seeds:** 1, 2, 3, 11, 23, and 99, each with both spawn assignments.
- **Shipping resolution:** rocky seeds 7 and 17, each with both spawn assignments.
- **Greedy opponent:** independent test policy that samples 24 directions along short strips, favors fresh grass and momentum, avoids rock, and boosts when the sampled strip is clear. It replans at 8 Hz and uses ordinary player inputs through the camera frame.

Seeds 3 and 99 were initially held out, then used to inspect and fix late-game stalls. These results are deterministic validation examples, not an untouched statistical evaluation or an estimate of win rate against people.

## Runtime measurements

Machine: Apple M2, 8 CPUs, 24 GiB RAM, macOS 15.6. Toolchain: rustc 1.93.1 and cargo 1.93.1. All measurements used `--release` with the repository's release profile, without a renderer. The final recorded cohorts and isolated benchmark ran sequentially after other CPU-heavy checks finished. Compilation and world setup are excluded from the measurements below.

For the four shipping-resolution matches, the reported **whole simulation tick** times were:

| Statistic | Range across the four matches |
| --- | ---: |
| Mean | 77–80 µs |
| p95 | 181–186 µs |
| p99 | 296–308 µs |

These tick measurements include rival AI, both vehicles' physics, mowing, scoring, and camera updates. The greedy player's input calculation runs outside the timed tick. They are not isolated AI costs. Simulation runs at 120 Hz; rival decisions run at 10 Hz.

The separate `benchmark_rival_decision_cost` unit benchmark measures only `RivalAi::decide`:

| Synthetic field | Mean decision | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Fresh | 278.502 µs | 402.292 µs | 406.167 µs |
| Fully covered | 142.996 µs | 297.917 µs | 383.375 µs |

That fixture uses a flat sphere with **64 mowing cells per face axis**, terrain resolution 24, fixed vehicle poses, and synthetic cruise velocity. It measures 300 decisions per condition, warms the coarse grass cache first, and allows a graph replan every tenth decision. It excludes physics, mowing updates, and rendering; its different synthetic state means its numbers should not be directly compared with the shipping tick percentiles. Local scheduling and hardware affect timing, so these measurements establish a practical budget rather than a portable timing guarantee.

The harness also prints movement on ticks with no new ownership, scoring droughts, and owned area per second. The movement metric includes cuts still below the ownership threshold; compare it at the same mowing resolution.

## Reproduce

Run these sequentially from the repository root. Environment overrides should be unset except those shown.

Original cohort, eight matches against each opponent:

```sh
cargo test -p lawn_core --release --test rival_competition benchmark_rival_competition -- --ignored --nocapture
```

Additional rocky seeds, twelve matches:

```sh
RIVAL_BENCH_SEEDS=1,2,3,11,23,99 RIVAL_BENCH_OPPONENT=greedy RIVAL_BENCH_TERRAIN=rocky cargo test -p lawn_core --release --test rival_competition benchmark_rival_competition -- --ignored --nocapture
```

Shipping resolution, four matches:

```sh
RIVAL_BENCH_SEEDS=7,17 RIVAL_BENCH_OPPONENT=greedy RIVAL_BENCH_TERRAIN=rocky RIVAL_BENCH_RESOLUTION=512 cargo test -p lawn_core --release --test rival_competition benchmark_rival_competition -- --ignored --nocapture
```

Isolated decision timing:

```sh
cargo test -p lawn_core --release --lib benchmark_rival_decision_cost -- --ignored --nocapture
```

Default competitive regressions:

```sh
cargo test -p lawn_core --test rival_competition -- --nocapture
```

The default suite requires at least three wins across four mirrored greedy matches, at least equal aggregate ownership, at least 45% rival ownership in each match, and scoring droughts below ten seconds. It also requires two mirrored wins against the frozen original driver. The full benchmark intentionally reports losses and unfinished matches without asserting universal wins.
