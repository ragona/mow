# Lawn Orbit

## Game Design and Implementation Specification

**Status:** Revised implementation specification  
**Genre:** Cozy driving / score-attack / lawn-mowing game  
**Mode:** Single-player  
**Target platform:** Desktop, landscape display  
**Primary input:** Gamepad; keyboard supported  
**Implementation language:** Rust  
**Rendering API:** `wgpu` with WGSL shaders  
**Target session length:** 2–8 minutes per planet  

---

## 1. High Concept

The player pilots a compact hover car around the entire surface of a procedurally generated, tiny spherical planet and mows its overgrown grass. The planet is small enough that the player can see its horizon curve sharply beneath the car, drive all the way around it in under a minute, and recognize rocky mountains appearing upside-down on the far side.

The generated planet is visually almost entirely lawn. Several rocky mountains and outcrops rise through the grass, creating recognizable landmarks, narrow passages, and large obstacles the player must route around. The uncut grass is rendered as genuine blade geometry, giving the planet a plush, fuzzy silhouette instead of the appearance of a green texture wrapped around a sphere.

The game combines:

- the tactile satisfaction of leaving clean mowing stripes;
- the momentum and expressiveness of an arcade driving game;
- the toy-like spectacle of navigating a complete miniature world; and
- a relaxed optimization challenge: cover the lawn efficiently while navigating the generated terrain.

The initial release should be a polished, replayable single-biome game with deterministic planet generation. Its central promise is simple: driving over real, visibly tall grass physically shortens it and permanently records the route.

### One-sentence pitch

Drive a hover mower around a fuzzy, pocket-sized planet, carving clean paths through its endless lawn and around its rocky mountains.

---

## 2. Product Goals

### Primary goals

1. Make the tiny planet convincingly fuzzy through visible grass geometry, especially along the horizon.
2. Make mowing immediately satisfying by physically shortening that geometry with strong visual and controller feedback.
3. Generate deterministic planets whose mountains create distinct but always viable mowing routes.
4. Make spherical traversal feel natural within the first 30 seconds.
5. Let players choose between relaxed completion and mastery through speed, precision, and route planning.
6. Keep the first implementation tightly scoped enough for a small team or coding agent to complete.

### Non-goals for the initial release

- Realistic vehicle or lawn-care simulation
- Open-world exploration beyond the planet
- Combat, enemies, or survival systems
- Character dialogue trees or a narrative campaign
- Vehicle construction or deep part customization
- Online multiplayer
- Hand-authored level-specific terrain layouts
- Settlements, characters, or decorative object simulation
- A reusable general-purpose game engine or editor
- Directly maintaining separate Vulkan, Metal, and Direct3D renderers
- Full physical simulation of individual grass blades
- Destructible terrain
- Mobile or touch-first controls

---

## 3. Design Pillars

### 3.1 A fuzzy whole world under your wheels

The planet must read as a small, grass-covered sphere rather than as a flat level with curved scenery. Real blades visibly extend beyond its horizon and separate through parallax as the camera moves. The player can circumnavigate the world in any direction, cross their own route, and see distant mountains rotate into view.

### 3.2 Every pass leaves a mark

Mowing physically shortens grass into visible stubble and produces a crisp, continuous trail whose width and location exactly match the mower deck. A completed lawn should visibly tell the story of the player's route.

### 3.3 Terrain creates the route

Procedurally placed mountain groups turn an otherwise open sphere into a routing problem. They should create useful landmarks and force broad detours without fragmenting the lawn into tedious pockets or trapping the vehicle.

### 3.4 Loose steering, precise results

The car should be easy to slide and turn, but the player must always understand where the mower will cut. Mistakes should feel attributable to the player's line, not unpredictable physics.

### 3.5 Cozy stakes, meaningful mastery

A player may finish slowly without failing. Skilled play is rewarded with better ratings for speed, coverage, clean lines, efficient routing, and avoiding collisions.

---

## 4. Audience and Tone

The game is suitable for a broad audience, including children, but should have enough movement depth and optimization to interest adults.

The tone is warm, compact, and lightly whimsical. The planet feels like a perfect, improbable ball of lawn punctured by ancient rock. Avoid parodying or directly reproducing characters, locations, or art from *The Little Prince*; the reference applies only to the scale and storybook feeling of a tiny personal planet.

There is no death. Falling away from the surface, becoming stuck, or overturning triggers a quick, friendly recovery.

---

## 5. Core Game Loop

1. **Generate and survey:** A seed produces the planet. The opening camera circles it and reveals its largest mountain groups, open lawns, and narrow routes.
2. **Mow:** The player drives freely and cuts tall grass beneath the active mower deck.
3. **Route:** The player chooses efficient paths around rock faces, through passes, and across already-cut areas while cleaning up missed patches.
4. **Finish:** The job becomes complete when required grass coverage reaches the target threshold.
5. **Review:** The game presents time, coverage, collisions, efficiency, and an animated map of the player's route.
6. **Replay or regenerate:** The player can retry the same seed for a better rating or generate a different planet.

The player is never forced to stop at the completion threshold. They may continue to 100% coverage before submitting the job.

---

## 6. Initial Playable Scope

### 6.1 Planet envelope

Planets have a base radius of approximately **15 meters**, giving them a nominal surface circumference of about **94 meters**. At ordinary top speed, a clean equatorial lap takes roughly **8 seconds**. The mower is intentionally large relative to the globe so speed makes the curvature dramatic.

Each generated planet should contain:

- **85–95% mowable grass** by surface area;
- broad, low-amplitude rolling grass terrain;
- **3–7 rocky mountain groups** of varying size;
- several smaller exposed-rock outcrops associated with those groups;
- multiple recognizable passes and routes between the mountain groups; and
- one flat, unobstructed starting region selected by the generator.

Mountains should be sparse enough that the player can develop speed over most of the world, but large enough to break sight lines and produce meaningful circumnavigation choices. Grass remains the dominant material from every broad view of the planet.

### 6.2 Seeded procedural generation

Planet generation is deterministic from the pair `(generator_version, world_seed)`. The same pair must reproduce the same:

- terrain displacement;
- mountain placement and shape;
- grass and rock classification;
- grass-root distribution;
- collision mesh;
- surface-area weights;
- starting location; and
- derived navigation metadata.

The generator should operate on a cube-sphere, subdivided icosphere, or another surface representation without gameplay-facing poles. All noise must be sampled from three-dimensional direction or position; applying ordinary two-dimensional noise to latitude and longitude is prohibited because it produces seams and polar distortion.

Recommended generation sequence:

1. Initialize a versioned seeded random stream.
2. Build the base spherical terrain mesh and broad rolling displacement.
3. Select mountain centers on the unit sphere using Poisson-disk or equivalent minimum-distance sampling.
4. Construct mountain mass from smooth radial profiles plus low-frequency ridged noise.
5. Derive the exposed-rock mask from mountain influence, elevation, and slope.
6. Classify the remaining valid surface as mowable grass.
7. Select a flat grass starting area away from rock.
8. Generate deterministic grass roots per surface patch.
9. Run playability validation; deterministically repair or regenerate invalid output.

### 6.3 Mountain requirements

Rocky mountains are integrated terrain masses, not isolated boulders placed on top of a sphere. Their collision shape should have a broad footprint and an unmistakable silhouette. Fine visual rock detail may exceed the collision mesh frequency.

Mountain generation must produce:

- a mixture of rounded mass and craggy exposed faces;
- clear grass-to-rock transitions;
- some grass-covered lower shoulders that remain drivable;
- steep upper faces that naturally repel or stop the hover car;
- clearance of at least two mower widths through intended passes; and
- no complete ring of impassable rock around the planet.

Nominal rolling terrain may vary by approximately 0.5–0.8 meters from the base radius. Mountain peaks may extend approximately 3–5.5 meters above it. These are initial tuning ranges.

### 6.4 Playability validation

Every generated planet must pass automated checks before play begins:

- at least 85% and no more than 95% of weighted surface area is mowable;
- at least 98% of required grass belongs to the largest mutually reachable grass region;
- the starting area meets configured radius, slope, and rock-clearance requirements;
- no required grass patch is narrower than the mower's practical access width;
- intended grass corridors meet minimum clearance;
- mountain collision does not create unrecoverable pockets; and
- the estimated ideal mowing time falls within the target session range.

Tiny disconnected or inaccessible grass islands should be reclassified as non-required rock rather than forcing the player to reach them. If deterministic repair cannot satisfy the constraints, advance an internal generation attempt counter derived from the same public seed and try again.

### 6.5 Topology

The planet has no invisible walls and no arbitrary poles. Any grass point can be approached from any direction allowed by generated rock terrain. The main lawn region must support more than one useful route around each major mountain group.

### 6.6 First-release content

The minimum shippable version includes:

- one deterministic planet generator and one grassland/rock biome;
- a fixed tutorial seed plus the ability to enter, copy, replay, and randomly generate seeds;
- one hover mower;
- a standard mowing job;
- a relaxed Free Mow mode;
- local best scores;
- keyboard and gamepad input;
- pause, settings, results, and restart flows; and
- a short first-play tutorial delivered through contextual prompts.

---

## 7. Player Vehicle

### 7.1 Form and silhouette

The vehicle is a small, rounded hover car with a visible front-mounted or belly-mounted mower deck. It should read as a charming utility machine rather than a weaponized racing vehicle. The deck's cutting width must be visually obvious from the driving camera.

### 7.2 Movement model

The vehicle maintains a fixed hover distance above the local terrain and aligns its up vector toward the surface normal. Its forward and lateral movement are constrained to the tangent plane at its current position.

Handling is arcade-like:

- strong acceleration at low speed;
- a moderate, readable top speed;
- speed-sensitive turning with a tight low-speed radius;
- mild lateral slip that allows controlled drifting;
- automatic stabilization after jumps or collisions;
- no manual gears;
- reverse speed lower than forward speed; and
- forgiving collision response with minimal pinballing.

The car may momentarily unload over bumps, but signed hover-pad suspension and strong radial attraction keep it glued to the lawn at boost speed. Nose-first tumbling is strongly damped, and the mower does not cut while it is too far above the lawn.

### 7.3 Recommended baseline tuning

These values are starting points and should be exposed as data rather than compiled constants.

| Parameter | Baseline |
| --- | ---: |
| Planet base radius | 15 m |
| Hover height | 0.8 m |
| Car length | 2.4 m |
| Mower cut width | 2.2 m |
| Maximum forward speed | 12 m/s |
| Maximum reverse speed | 4 m/s |
| Time to 90% top speed | 0.9 s |
| Full-speed turn radius | 6 m |
| Low-speed turn radius | 2 m |
| Steering strength | 1.0 |
| Boost maximum speed | 28 m/s |
| Recovery hold time | 1.0 s |

### 7.4 Boost

The vehicle has a short rechargeable boost. Boost increases thrust and top speed but makes precise mowing more difficult. The boost meter refills automatically after a short delay, encouraging expressive use without introducing consumable-resource anxiety.

Boost is optional for completion and can be disabled in accessibility settings.

### 7.5 Recovery

The player can hold the recovery input at any time. Recovery fades the screen briefly and places the car at the nearest safe surface point, facing roughly along its prior direction. Automatic recovery triggers if the car remains outside a valid distance band, upside-down, or nearly motionless against an obstacle for several seconds.

Recovery adds a small time penalty in rated modes but never removes progress.

---

## 8. Controls

### 8.1 Gamepad

| Action | Default input |
| --- | --- |
| Strafe with gentle auto-yaw | Left stick |
| Accelerate | Right trigger |
| Brake / reverse | Left trigger |
| Boost | South face button |
| Look behind | North face button |
| Recover vehicle | Hold East face button |
| Camera orbit | Right stick |
| Recenter camera | Right stick click |
| Pause | Menu button |

### 8.2 Keyboard

| Action | Default input |
| --- | --- |
| Strafe with gentle auto-yaw | A / D or Left / Right |
| Accelerate | W or Up |
| Brake / reverse | S or Down |
| Boost | Space |
| Look behind | Q |
| Recover vehicle | Hold R |
| Camera orbit | Mouse movement while held, or alternate keys |
| Recenter camera | C |
| Pause | Escape |

All gameplay controls must be remappable. Analog trigger input should be preserved rather than converted to binary values.

### 8.3 Mower behavior

The mower is always enabled and cuts whenever its deck is close enough to mowable terrain. Deck animation, particles, and controller feedback communicate when it is actively contacting tall grass or harmlessly crossing rock.

---

## 9. Mowing System

### 9.1 Surface state

Each point on mowable terrain has a persistent cut state from 0 to 1:

- `0.0`: fully grown;
- intermediate values: partially cut or visually transitioning; and
- `1.0`: fully mowed.

Gameplay coverage calculations may treat a cell as cut when it reaches at least `0.9`.

Exposed rock and other non-grass surfaces are represented separately and do not count toward the denominator for required lawn coverage.

### 9.2 Cutting footprint

While the mower is close enough to the ground, the game projects a rectangular or capsule-shaped cutting footprint from the deck onto the planet surface. Every touched mowable sample advances toward the cut state, including at full boost speed.

The cutting footprint must be sampled continuously between physics frames so fast movement cannot leave dotted gaps. A practical rule is to stamp the mask at intervals no greater than one quarter of the deck width along the traveled path.

The mower should still cut while reversing, at reduced effectiveness if needed for balance.

### 9.3 Geometric cutting result

Mowing must alter grass geometry rather than merely painting the ground. Each grass root samples the authoritative cut field, and the blade vertices shorten from their uncut height to a small nonzero stubble height. A cut patch should therefore have:

- physically shorter blades with visible geometric stubble;
- a directional lean that creates a lighter or darker stripe through lighting;
- a clean boundary aligned with the deck;
- a brief spray of clippings near the deck; and
- short-lived deformation where the mower and hover wash bend grass.

The ground material may reinforce the state, but it must not be the primary source of apparent height, striping, or the cut boundary. The difference between tall grass and mowed lawn must remain clear in silhouette, parallax, and lighting with ground color held constant.

### 9.4 Authoritative mowing field

The reference implementation should store mowing state in a six-face cube-map field or an equivalent seam-safe per-patch field. A cube map is preferred because grass roots and mower stamps can address it from normalized planet-centered directions without gameplay-facing poles.

The field should retain at least:

| Value | Purpose |
| --- | --- |
| Cut amount | Authoritative uncut-to-stubble state and coverage calculation |
| Tangent comb direction | Direction surviving blades lean after the mower passes |
| Recent-cut time | Brief clipping, settling, and color-response animation |

Store comb direction as a world-space tangent vector, using a compact encoding such as octahedral encoding, or explicitly transform face-local directions at cube boundaries. The field needs enough effective resolution that a 2.2-meter mower deck spans at least 6–8 samples. For a 15-meter-radius sphere, a **512×512 field per cube face** provides ample upper-quality headroom; a lower resolution may be used during prototyping. Sampling and stamping across cube-face boundaries must be seamless.

### 9.5 Coverage accounting

Coverage must be calculated from actual mowable surface area, not raw texture pixels if those pixels represent unequal areas. The generator should precompute a weighted sample table for each planet.

Display coverage rounded down to one decimal place so the HUD never announces completion before the authoritative value reaches the target.

### 9.6 Rock interaction

Exposed rock cannot be mowed and does not contribute to coverage. The mower should harmlessly spark if its footprint crosses rock, while the vehicle body responds to mountain collisions normally.

Rated modes record substantial rock collisions, but glancing contact should not stop the run or destroy the vehicle. Collision feedback should be clear without turning the game punitive.

---

## 10. Objectives, Completion, and Scoring

### 10.1 Standard job

The standard job requires at least **98.0%** of the generated planet's mowable grass to be cut.

The final 2% tolerance prevents the player from hunting indefinitely for tiny invisible slivers. A “nearby uncut grass” assist becomes available after 95% coverage.

### 10.2 Score categories

The results screen reports categories separately instead of hiding everything behind one opaque score.

| Category | Measurement |
| --- | --- |
| Coverage | Percentage of mowable area cut |
| Time | Seconds from gaining control to submission |
| Control | Number and severity of collisions with rocky terrain |
| Efficiency | Total distance traveled relative to estimated ideal coverage distance |
| Recoveries | Manual and automatic recoveries |

The game also awards an overall rating from one to three stars. Completion always awards at least one star.

Suggested initial thresholds:

- **1 star:** Complete the job.
- **2 stars:** Complete under 18 minutes with at most two substantial collisions.
- **3 stars:** Complete under 13 minutes, reach at least 99.5% coverage, and record no substantial collisions or recovery events.

These are tuning values, not contractual difficulty targets.

### 10.3 Free Mow

Free Mow removes the timer, rating, collision penalties, and forced results flow. The player can reset the grass or generate a new seed at any time. This mode is available from the start.

### 10.4 Optional medals

Post-MVP challenges may include:

- no overlapping passes beyond a tolerance;
- mow without boost;
- complete using cockpit camera;
- complete without contacting exposed rock;
- cut a continuous spiral; and
- complete without using recovery.

---

## 11. Camera

### 11.1 Standard camera

The current camera is a fully top-down local-radial view positioned directly above the vehicle. It looks straight into the planet, uses the travel direction as screen-up, and uses an undistorted 90-degree perspective projection. Horizontal camera input rotates screen heading while vertical input adjusts height without tilting the view.

The camera should favor the vehicle's velocity direction over its nose during a drift. It must not snap at geographic poles because the planet has no gameplay-facing longitude frame.

### 11.2 Collision and obstruction

Camera obstruction is resolved by moving the camera toward the vehicle along its desired boom, not by allowing geometry to become opaque. Transitions in and out of obstruction should be damped to prevent popping.

### 11.3 Additional views

The initial release may include:

- a closer bumper camera;
- a high “toy camera” that emphasizes the full planet; and
- a look-behind view.

Only the standard camera is required for MVP.

### 11.4 Motion comfort

The camera must offer:

- adjustable camera shake, including off;
- adjustable field of view;
- adjustable camera follow stiffness;
- an optional fixed-horizon mode if testing shows surface rotation causes discomfort; and
- no involuntary camera roll beyond what is necessary to track the local surface.

---

## 12. User Interface

### 12.1 In-game HUD

Keep the HUD small and readable. It contains:

- lawn coverage percentage;
- elapsed time in rated jobs;
- active cutting feedback;
- boost meter;
- substantial-collision count; and
- contextual prompts during the tutorial.

A miniature globe map is not required during ordinary play. After 95% coverage, an optional locator can point toward the largest nearby uncut region.

### 12.2 Results screen

The results screen shows:

- overall rating;
- final coverage;
- completion time;
- collisions and recoveries;
- efficiency;
- personal-best comparisons; and
- retry, Free Mow, and menu actions.

The centerpiece is a slowly rotating planet displaying the final cut pattern. If practical, replay the mowing field filling in over time using recorded deck stamps.

### 12.3 Menus

Required menus:

- title screen;
- mode selection;
- pause;
- settings;
- results; and
- confirmation for reset or return-to-menu actions that discard an active run.

---

## 13. Tutorial and Onboarding

The tutorial occurs inside the first standard job and does not require a separate level.

1. Prompt the player to accelerate and steer.
2. Once moving, call attention to the active mower and rising coverage value.
3. Introduce boost on a clear stretch of grass.
4. Guide the player around a rocky mountain and explain which slopes and surfaces are not mowable.
5. Explain recovery only after the player becomes stuck or opens the pause menu.
6. At 95% coverage, introduce the uncut-grass locator.
7. At 98%, explain that the player can submit or continue to 100%.

Prompts disappear immediately after the corresponding action and do not repeat on later runs unless tutorials are reset in settings.

---

## 14. Art Direction

### 14.1 World

Use a stylized, storybook miniature aesthetic with simplified forms, soft lighting, and strong material separation between tall grass, cut grass, and exposed rock. The sphere should look deliberately tiny rather than like a distant realistic planet.

Key visual cues:

- exaggerated horizon curvature;
- a clean atmospheric rim or soft halo;
- a sparse sky with a few large celestial objects;
- generated mountain silhouettes readable from across the globe;
- wind movement in uncut grass; and
- restrained, toy-like proportions.

### 14.2 Grass

Grass is the planet's defining visual feature and must be rendered as geometry wherever individual height or silhouette is perceptible. A surface texture may provide soil color, low-frequency variation, and distant support, but may not substitute for visible blades.

The preferred primitive is an instanced tuft containing approximately 3–5 opaque, tapered blades. Each blade should have two or three bend segments and enough width to remain stable under antialiasing. Avoid making transparent billboard cards the primary representation; their overdraw, intersection patterns, and edge behavior work against the plush miniature look.

The grass should exhibit:

- a visibly irregular, fuzzy planetary horizon;
- parallax between nearby blades as the camera moves;
- modest correlated variation in height, lean, width, and color;
- darker roots or inexpensive root occlusion;
- subtle light transmission or wrap lighting at backlit tips;
- coherent wind traveling through the local tangent field;
- localized bending from hover wash and the mower; and
- short geometric stubble after mowing.

The current exaggerated presentation scales uncut blades to approximately 1.2–1.9 meters tall while retaining roughly 8.5-centimeter stubble, making every cut path dramatically legible from the top-down camera.

Variation should occur in patches as well as per blade. Fully independent random color and motion will resemble visual noise rather than vegetation.

### 14.3 Mowing appearance

The primary mowing transition is geometric: tall blades bend toward the deck, shorten, and settle into stubble. Clipping particles briefly continue along the vehicle's velocity and local wind direction.

Mowing stripes come primarily from the comb direction written by the mower. Short blades lean along that tangent direction, changing their normals and response to the light. Painted light and dark stripes may reinforce this effect, but must not create it alone.

### 14.4 Vehicle feedback

The hover car communicates forces through:

- body lean during steering;
- deck vibration while cutting;
- hover-pad compression or light intensity;
- dust and clipping direction tied to velocity;
- brake and reverse lights; and
- a short squash-and-settle response on landing.

---

## 15. Audio Direction

Audio is intentionally omitted from the current prototype. Gameplay feedback is visual and, when available, reinforced by controller rumble. A future sound pass should begin from playtested recordings rather than retaining placeholder synthesis.

---

## 16. Accessibility and Player Options

Required options:

- full input remapping;
- camera shake slider;
- field-of-view adjustment;
- steering sensitivity and inversion;
- hold/toggle options where applicable;
- high-contrast cut-grass mode;
- reduced particle mode;
- relaxed completion mode with no timer or penalties; and
- an enlarged uncut-grass locator.

Avoid communicating mower state, grass state, or rock boundaries through color alone.

---

## 17. Game States and Saved Data

### 17.1 Top-level game states

```text
Boot -> Title -> Mode Select -> Loading -> Playing -> Results
                                      |-> Paused -> Playing
                                      |-> Paused -> Mode Select
```

### 17.2 Run state

A run contains at least:

- generator version and world seed;
- mode;
- elapsed active time;
- current mowing field;
- weighted coverage value;
- vehicle transform and velocity;
- mower and boost state;
- substantial collision events;
- recovery count;
- distance traveled;
- tutorial progress; and
- optional compact history of mowing stamps for results playback.

### 17.3 Persistent profile

Persist:

- settings and control bindings;
- tutorial completion;
- unlocked content;
- recently played and favorited seeds;
- best rating by seed;
- best time by seed;
- highest coverage by seed; and
- best efficiency by seed.

An active run may be saved on clean exit, but mid-run persistence is not required for MVP because sessions are short.

---

## 18. Technical Architecture

### 18.1 Architecture decision

The game will use a purpose-built Rust runtime directly on `wgpu`. This is a game-specific micro-engine, not a reusable general engine. The project owns the frame loop, renderer, spherical terrain, mowing model, grass simulation, hover controller, and game flow while reusing focused libraries for operating-system integration, collision solving, math, serialization, and development tooling.

The project must not begin from Unity, Unreal, Godot, full Bevy, or another general-purpose engine. It must also not implement separate Vulkan, Metal, and Direct3D backends. `wgpu` is the graphics abstraction boundary.

This hybrid boundary exists to maximize control over the unusual systems without spending the project on window creation, controller databases, collision detection, or graphics portability.

### 18.2 Foundation libraries

| Concern | Foundation | Usage |
| --- | --- | --- |
| Graphics and compute | `wgpu` | Device selection, resources, command encoding, compute, render passes, and presentation |
| Shaders | WGSL | All shipping vertex, fragment, and compute shaders |
| Windows and events | `winit` | Desktop window, keyboard, mouse, resize, focus, and event loop |
| Gamepads | `gilrs` | Unified controls, hot-plugging, mappings, and rumble |
| Math | `glam` | Vectors, matrices, quaternions, and transforms |
| Collision and rigid bodies | `rapier3d` | Static terrain collision, one dynamic vehicle, queries, contact impulses, and CCD |
| Development UI | `egui` with `egui_wgpu` | Runtime tuning, seed tools, render inspection, and profiling overlays |
| Data | `serde` with RON or JSON | Tunable configuration, profile data, seeds, and diagnostic captures |
| Diagnostics | `tracing` plus GPU timestamp queries | Structured logs, CPU spans, and GPU pass timing |

Pin dependency versions in the lockfile and upgrade intentionally after running performance and regression tests. Do not depend on experimental mesh shaders, bindless resources, or other optional GPU features for the baseline renderer.

An ECS is not required initially. Ordinary game state should use explicit Rust structs and subsystem ownership. Grass roots, terrain patches, mowing cells, and particles must never be represented as ECS entities. If later content growth makes ECS valuable, `bevy_ecs` may be introduced behind gameplay-facing interfaces without adopting the Bevy renderer.

### 18.3 Runtime and frame loop

Keep scheduling explicit. The main thread owns platform events and presentation. The simulation advances at a fixed **120 Hz**; rendering runs once per display frame and interpolates between the two latest simulation transforms.

Each displayed frame performs the following logical sequence:

1. Pump platform and controller events into an immutable input snapshot.
2. Run zero or more fixed simulation ticks to catch up with real time, subject to a maximum catch-up limit.
3. Update vehicle physics and stamp the CPU-authoritative mowing field during each simulation tick.
4. Upload frame uniforms, dirty mowing tiles, vehicle transforms, and grass force sources.
5. Run transient grass-interaction compute work.
6. Encode and submit the render passes.
7. Present, collect completed timing results, and update diagnostics.

Planet generation and asset decoding may use background worker tasks because they occur outside active play. Avoid a generalized per-frame job system until profiling identifies CPU work large enough to justify one. No frame-critical path may wait on asset I/O.

### 18.4 Coordinate model

Use a planet-centered world frame. At vehicle position **p** relative to planet center **c**, the nominal up vector is:

```text
up = normalize(p - c)
```

Project desired movement and steering forces onto the local tangent plane:

```text
tangent(v) = v - dot(v, up) * up
```

When terrain has generated displacement, obtain the hover target and normal from a radial surface query or local collision query. Do not rely on a single global gravity vector.

### 18.5 Vehicle simulation and collision

A single dynamic `rapier3d` rigid body represents the hover mower. Set conventional global gravity to zero and apply all gravity, hover, propulsion, grip, and stabilization forces explicitly. Rapier owns collision detection, contact impulses, and continuous collision detection; game code owns the vehicle's desired behavior.

Use four hover-pad anchors distributed around the vehicle hull. Each pad casts toward the planet surface and applies a spring-damper force at its world-space anchor:

```text
hover_force = clamp(k * (target_height - hit_distance)
                  - c * normal_velocity,
                    0,
                    maximum_hover_force)
```

Applying force at each pad naturally creates pitch and roll. A separate proportional-derivative torque aligns the vehicle's up vector toward the averaged contacted surface normal while still permitting readable banking and temporary airborne rotation.

Each simulation tick should:

1. query surface distance and local normal;
2. apply spring-damper hover force;
3. apply attraction toward the surface;
4. align vehicle up toward the local normal with damped torque;
5. project drive, braking, and lateral grip forces into the tangent plane;
6. cap or asymptotically limit speed; and
7. use shape casts or CCD to prevent high-speed tunneling into mountain collision;
8. collect substantial contact impulses for scoring and rumble; and
9. update mower sampling from the deck's swept path.

Use a simple convex hull or small compound collider for the vehicle and a lower-frequency static triangle mesh for the generated planet. Do not use Rapier's character controller: its translation-oriented abstraction cannot supply the rotational dynamics required here.

Keep Rapier behind a narrow `PhysicsWorld` and `VehiclePhysics` interface. If full rigid-body behavior later proves too difficult to tune, this boundary permits replacing vehicle integration with custom kinematics plus Parry shape queries without disturbing gameplay or rendering code.

### 18.6 CPU procedural planet generation

Represent terrain as surface patches over a cube-sphere, subdivided icosphere, or equivalent seam-safe sphere. Evaluate broad terrain and mountain functions using normalized three-dimensional directions. Generation must not depend on latitude-longitude UV coordinates.

Generate terrain, classification, collision geometry, surface-area weights, and grass roots on the CPU when loading a planet. The GPU must not be the authoritative generator: physics, scoring, save/load, reproducibility, and validation all need the same data without readback.

A useful initial displacement model is:

```text
surface_radius(d) = base_radius
                  + rolling_noise(d)
                  + sum(mountain_profile(d, mountain_i))
```

where `d` is the unit direction from the planet center. Mountain centers should be distributed with a minimum angular separation. Each mountain combines a smooth radial mass with ridged, lower-amplitude detail; the smooth mass determines gameplay collision, while finer detail may be visual only.

Generation should be divided into explicit stages with independently derived random streams. Adding cosmetic grass variation in a later generator version must not inadvertently move every mountain. Store the generator version with every seed and saved run.

The full-resolution render mesh and lower-frequency collision mesh derive from the same sampled height data. Upload immutable render patches, collision geometry, and grass-root buffers after generation. Release temporary generation data that is not required for mowing, recovery, or regeneration diagnostics.

### 18.7 Terrain classification and collisions

The generator derives grass and rock from mountain influence, elevation, and local slope. Classification should be morphological rather than noisy at the mower scale: eliminate tiny rock specks, narrow grass slivers, and boundaries the vehicle cannot read at speed.

The visual planet may contain fine displacement, but collision geometry should remain smooth and low-frequency. The exposed-rock boundary used by mowing must agree closely with the visible material and collision surface. Avoid collision seams at cube-face or mesh-section boundaries.

After classification, build a coarse connectivity graph over traversable grass cells. Use it for validation, spawn selection, recovery locations, and the late-run uncut-grass locator.

### 18.8 Grass-root distribution and geometry

Partition the sphere into independently cullable surface patches. Within each mowable patch, generate deterministic grass roots using blue-noise, low-discrepancy, or similarly even seeded placement. A root stores or reproducibly derives:

- its position on the generated terrain;
- its local surface normal;
- a random rotation in the tangent plane;
- tuft height, width, and lean variation; and
- a compact variation or species index.

Each rendered instance is a small fixed-topology tuft mesh, preferably 3–5 opaque tapered blades with two or three bend segments. The vertex shader constructs the tuft in a local tangent frame and applies height variation, wind, interaction, and cut state. A conceptual compact root record is:

```rust
#[repr(C)]
struct GrassRootGpu {
    position: [f32; 3],
    packed_normal_seed: u32,
}
```

This 16-byte representation allows approximately 400,000 roots to occupy about 6.4 MiB on the shipping-size planet. Patch-relative quantization is allowed if measurement shows a useful bandwidth or memory improvement, but a more complex representation is not required initially.

Construct the tangent frame locally without geographic coordinates. One robust method selects the Cartesian axis least aligned with the surface normal, crosses it with the normal to produce the first tangent, and derives the second tangent by another cross product. Apply the root's random rotation within that frame.

The ground material is never authoritative for apparent grass height. Holding the ground color constant must still reveal tall grass, cut stubble, mowing boundaries, and the fuzzy horizon.

Grass roots, terrain patches, mowing cells, and clipping particles are GPU-oriented arrays, not gameplay objects or ECS entities.

### 18.9 Grass culling and level of detail

The small planet makes genuine geometry practical because a near-surface camera sees only a spherical cap. On a 15-meter-radius planet, a camera approximately 5–8 meters above the surface sees roughly 470–750 square meters of terrain, or about 17–27% of the full sphere.

Cull grass at the patch level against the camera frustum and geometric horizon. Submit visible patches through batched instancing or GPU-generated indirect draws; do not issue one draw call per tuft.

Recommended LOD behavior:

| Region | Representation |
| --- | --- |
| Near camera | Full segmented tuft geometry with interaction and detailed lighting |
| Midrange | Lower-segment or lower-blade-count tuft geometry |
| Geometric horizon | Sparse but still real geometry preserving full apparent blade height |
| High toy-camera view | Aggressively thinned and modestly widened tuft geometry over supporting ground shading |

Reduce density stochastically as projected size falls, with stable decisions derived from root identity. Slightly widen retained blades to preserve visual mass, but do not shrink their height near the horizon; shrinking collapses the fuzzy silhouette.

Transparent foliage cards, fur shells, and parallax ground materials may be used only as distant support. None may replace real geometry at the planet's visible limb in ordinary gameplay views.

### 18.10 Grass lighting and transient interaction

Grass lighting should be inexpensive but materially distinct from the ground. Blend blade normals toward the local surface normal near the root, retain directional blade normals toward the tip, and provide modest root occlusion plus a subtle backlighting or transmission term.

Wind is a low-frequency vector field evaluated in each root's tangent plane, not a displacement along a global world axis. Neighboring patches must sample the same continuous wind field.

Dynamic vehicle interaction uses a lower-resolution GPU field separate from the permanent mowing field. Begin with **128×128 samples per cube face** and two logical world-space `RGBA16F` fields containing tangent displacement and tangent velocity. Double-buffer them if the selected storage path cannot safely read and write the same resources. Four physical textures at this resolution consume approximately 3 MiB.

Each rendered frame uploads a small force-source buffer containing:

- one downwash source for each hover pad;
- a broader body-wash source;
- mower suction and downward force;
- an elongated wake aligned opposite tangent velocity; and
- a bounded ring of recent trail capsules when required for longer recovery motion.

A compute pass integrates a damped spring independently at every field sample:

```text
acceleration = applied_force
             - stiffness * displacement
             - damping * velocity

velocity     += acceleration * dt
displacement += velocity * dt
```

Project both vectors onto the sample's local tangent plane after integration. Force falloff should be smooth and evaluated from three-dimensional planet-centered positions so interaction remains continuous across cube-face boundaries.

The grass vertex shader samples the resulting displacement and applies it increasingly toward the blade tip:

```text
blade_offset = displacement * normalized_blade_height^2
```

The intended response is that hover pads flatten grass outward, the moving body streams it backward, mower suction pulls it inward and down immediately before cutting, and recently disturbed grass oscillates briefly before settling. These effects are entirely visual and must not modify authoritative cut state or scoring.

If the baseline adapter cannot use the preferred storage texture format, provide a storage-buffer implementation of the same logical field. Direct analytical evaluation of a reduced force-source list in the vertex shader is an acceptable final fallback.

### 18.11 Authoritative mowing state and geometry

The CPU owns a six-face mowing grid, initially **512×512 cells per face**. A four-byte packed cell occupies approximately 6 MiB for the entire planet and should encode cut amount, tangent comb direction, and a compact recent-cut value. Coverage, save data, and results are derived from this CPU representation; the game must never require GPU readback to know what has been mowed.

Every fixed simulation tick stamps the mower deck's swept capsule between its previous and current poses. Stamping resolves samples from normalized three-dimensional directions so a pass crossing a cube boundary remains continuous. Track modified cells in dirty tiles and upload only those tiles to the matching GPU texture before rendering.

Grass roots sample the GPU mirror using their normalized planet-centered direction. At minimum, the vertex shader maps cut amount to blade height:

```text
blade_height = mix(uncut_height, stubble_height, cut_amount)
```

`stubble_height` must remain greater than zero. The comb direction rotates or bends the surviving blade sections in the local tangent plane, producing mowing stripes through geometric normals and lighting. Recent-cut time controls only temporary settling, color response, and clipping emission.

### 18.12 Render pipeline

Use a compact forward renderer built around one dominant directional light. The baseline frame contains:

1. transient grass-interaction compute;
2. one directional shadow pass for terrain, mountains, and the vehicle;
3. opaque terrain, mountains, and vehicle;
4. opaque geometric grass;
5. clipping particles and other transparent effects;
6. tone mapping and world upscaling; and
7. user interface.

Individual grass blades do not cast shadows in the baseline or low-quality paths. Grass receives terrain and mountain shadows, while root darkening, directional blade normals, and inexpensive transmission provide local depth. A short-range grass shadow option may be tested for higher quality but cannot become necessary for the intended appearance.

Do not begin with deferred shading, screen-space reflections, screen-space ambient occlusion, volumetric fog, or a generalized render graph. The scene has one important light and a deliberately narrow set of passes. Encode them explicitly until a demonstrated requirement justifies more infrastructure.

### 18.13 Capability tiers and draw submission

Query adapter capabilities at startup and select one of the following paths:

| Tier | Required behavior |
| --- | --- |
| Baseline | Compute grass field, CPU surface-patch culling, standard instanced draws |
| Enhanced | GPU visibility compaction plus indirect or multi-draw-indirect submission |
| Experimental | Mesh-shader tuft generation or culling experiments |

The baseline path is the shipping requirement. Begin implementation with CPU frustum and geometric-horizon culling over a few hundred or few thousand patches. One instanced draw per visible patch and LOD is acceptable until profiling proves otherwise.

Organize root buffers and patch metadata so an enhanced compute pass can later compact visible root indices by LOD and write indirect arguments without changing grass shading or gameplay data. Mesh shaders are never a minimum requirement and must have no unique visual behavior.

### 18.14 Antialiasing and image stability

Thin vegetation must remain stable in motion. Prevent subpixel geometry before trying to repair it in post-processing:

- thin roots stochastically using stable root identities;
- modestly widen retained distant blades;
- preserve apparent blade height at the horizon;
- avoid alpha-tested billboard forests; and
- expose grass density separately from render resolution.

Begin with **2× MSAA** in the low preset and **4× MSAA** in the standard/high presets. Temporal antialiasing is deferred until the visual prototype demonstrates a need; if introduced, it must include animated-grass motion vectors and a responsive mask for newly cut blades to avoid ghosting.

### 18.15 Determinism, budgets, and performance

The public world seed and generator version must reproduce terrain, classification, grass roots, spawn, and validation result. Exact cross-machine deterministic vehicle physics is not required. Coverage and score accounting must, however, be stable within a run and independent of render frame rate.

Initial standard-quality rendering budgets should aim for roughly **150,000–300,000 visible tuft instances** at the ordinary chase-camera height. The provisional low path should support approximately **75,000–150,000 visible tufts** through stable density reduction. These are measurement targets, not requirements to distribute grass uniformly or retain invisible instances.

Use provisional performance tiers until the visual prototype establishes final minimum hardware:

| Profile | Provisional hardware class | Requirement |
| --- | --- | --- |
| Low | Intel UHD 620-class integrated graphics | 60 FPS at 1280×720 or 1600×900 internal resolution |
| Standard | Intel Iris Xe, Apple M1, or Ryzen 680M-class graphics | 60 FPS at 1920×1080 |

Do not promise 1080p60 on the low profile until it has been measured. World rendering may use dynamic or preset internal resolution while UI remains at native resolution.

At 60 FPS the total frame budget is 16.67 ms. Design for a normal frame under 14 ms on the target device, leaving transient headroom. Initial subsystem budgets are:

| Work | Budget |
| --- | ---: |
| Up to two 120 Hz simulation ticks | 2.0 ms CPU |
| Transient grass-field compute | 1.0 ms GPU |
| Grass draw | 6.0 ms GPU |
| Remaining scene, effects, post, and UI | 5.0 ms GPU |

In addition:

- fixed physics must remain at 120 Hz on every quality preset;
- stable thin-blade antialiasing without distracting shimmer;
- no visible hitch when mowing, crossing patch boundaries, or changing LOD;
- generated-planet load under 10 seconds from a warm application start; and
- input-to-motion latency appropriate for an arcade driving game.

Instrument named CPU spans and GPU passes from the first visual prototype. Record frame-time percentiles, visible patches, visible tufts, generated triangles, mowing uploads, interaction-field cost, and transient allocation counts. Quality should scale through grass density, render scale, MSAA, particle count, and optional shadow features—not by reducing physics rate or changing mowing results.

---

## 19. Suggested Component Boundaries

| Component | Responsibility |
| --- | --- |
| `Platform` | `winit` event loop, window state, timing, lifecycle, and presentation coordination |
| `Input` | Keyboard, mouse, `gilrs` gamepads, mappings, input snapshots, and rumble |
| `GameFlow` | Menus, loading, playing, pause, completion, and results transitions |
| `PlanetGenerator` | Versioned seeded terrain, mountain placement, classification, grass roots, and spawn candidates |
| `PlanetValidator` | Coverage ratio, connectivity, clearance, accessibility, and time-budget checks |
| `PlanetSurface` | Runtime surface queries, normals, terrain metadata, connectivity, and safe recovery locations |
| `PhysicsWorld` | Rapier ownership, static terrain collider, fixed stepping, queries, contacts, and interpolation state |
| `HoverVehicle` | Input interpretation, radial attraction, hover forces, propulsion, grip, stabilization, and recovery |
| `MowerDeck` | Activation, valid-cut checks, and swept cutting footprint |
| `MowingField` | Persistent cut amount, comb direction, timestamps, weighted coverage, and seam-safe stamps |
| `GrassRootField` | Deterministic per-patch root placement and compact tuft attributes |
| `GrassInteraction` | GPU displacement/velocity fields, force-source upload, spring integration, and fallback path |
| `Renderer` | `wgpu` ownership, resources, explicit passes, instanced grass, culling, LOD, lighting, effects, and presentation |
| `ObjectiveSystem` | Completion rules and optional job objectives |
| `ScoreSystem` | Time, coverage, collisions, efficiency, recoveries, ratings, and per-seed records |
| `CameraRig` | Top-down camera, local-up tracking, rotation, zoom, and comfort options |
| `SaveProfile` | Settings, bindings, tutorial flags, unlocks, and records |
| `RunRecorder` | Optional compact mowing-stamp history for result playback |
| `DevTools` | `egui` tuning, seed inspection, debug views, timing graphs, and diagnostic capture |

Components should communicate through explicit state and events. Rendering code must not be the source of truth for coverage or scoring.

### 19.1 Suggested Cargo workspace

Avoid fragmenting the prototype into many tiny crates. Begin with:

| Crate | Contents |
| --- | --- |
| `lawn_orbit` | Binary, platform loop, input, game flow, and composition root |
| `lawn_core` | Seeded generation, planet data, mowing, scoring, vehicle state, and physics integration |
| `lawn_render` | `wgpu`, WGSL shaders, GPU resources, grass interaction, frame encoding, and debug rendering |
| `lawn_tools` | Optional generator fuzzing, asset conversion, captures, and standalone benchmarks |

`lawn_core` must remain usable by headless tests and generator-fuzzing tools. `lawn_render` consumes immutable snapshots and explicit upload commands rather than reaching into mutable gameplay state. Do not create a separate generic engine crate.

---

## 20. MVP Acceptance Criteria

The MVP is complete when all of the following are true:

### Runtime and technology

- The shipping runtime and gameplay code are written in Rust and render directly through `wgpu` using WGSL shaders.
- No full general-purpose engine or project-specific generic engine layer is required to run the game.
- The application detects adapter capabilities and successfully selects the baseline path without enhanced indirect-draw features.
- Simulation advances at a fixed 120 Hz and rendered transforms interpolate without tying game speed to display rate.
- `lawn_core` generation, mowing, validation, and score tests run headlessly without creating a GPU device or window.
- Authoritative mowing and scoring never require GPU readback.
- Grass roots and blades have no per-instance CPU update during ordinary frames.

### Procedural planet

- `(generator_version, world_seed)` reproduces identical terrain, classification, grass roots, spawn, and validation output.
- Generated planets contain the configured 85–95% weighted mowable area and 3–7 mountain groups.
- At least 98% of required grass is mutually reachable from the starting region.
- Mountain groups create meaningful detours without encircling the planet or producing unrecoverable pockets.
- A batch test of at least 1,000 arbitrary seeds generates playable planets without manual intervention.
- The tutorial seed is produced by the same generator rather than by a separate authored level.

### Planet traversal

- The player can drive continuously around the planet in any direction.
- Vehicle orientation remains stable through a complete circumnavigation and across arbitrary poles.
- The camera does not snap or invert unexpectedly.
- The player cannot permanently lose the vehicle in space or become irrecoverably stuck.
- Intended mountain passes are readable and wide enough for controlled mowing.

### Grass rendering

- Uncut grass is visible blade or tuft geometry in the ordinary chase camera.
- Grass geometry breaks the planet's silhouette continuously along the visible horizon.
- Uncut grass still reads as tall, cut grass as stubble, and the mowing boundary as crisp when the supporting ground color is held constant.
- Nearby grass exhibits parallax, correlated variation, wind, and localized hover deformation.
- Hover-pad downwash bends grass outward, vehicle motion produces a readable wake, mower suction bends grass immediately before cutting, and disturbed grass settles rather than snapping upright.
- Transient bending remains continuous across grass patches and cube faces and never changes authoritative coverage.
- Mowing stripes derive visibly from geometric height or comb direction rather than solely from painted albedo.
- Patch culling and LOD transitions do not create bald rings, synchronized popping, or a collapsing horizon.
- Thin blades remain acceptably stable under motion at the target resolution.

### Mowing

- Driving the active mower over tall grass produces a continuous cut trail matching the visible deck width.
- Cutting physically shortens grass instances to nonzero geometric stubble.
- Cut state persists for the duration of the run.
- Repeated passes do not incorrectly increase coverage.
- Cube faces, surface patches, and arbitrary poles do not create uncuttable strips, duplicate coverage, bald seams, or visible discontinuities.
- Coverage is independent of render frame rate.
- Exposed rock never contributes to the required coverage denominator.

### Objectives

- The job becomes complete at the configured coverage threshold.
- Substantial collisions with mountain terrain are detected and reported.
- Results accurately report seed, coverage, time, collisions, distance, and recoveries.
- Restarting resets all run state and grass state.
- Retrying preserves the seed; generating a new planet changes it.

### User experience

- A first-time player can discover driving, mowing, boosting, and completion without external instructions.
- Keyboard and gamepad can complete every flow.
- Pause and settings function during gameplay.
- Free Mow, the standard job, replay seed, copy seed, and new random seed are accessible from the title or results flow.
- The game maintains the selected profile's target frame rate while mowing dense grass beside a mountain at the visible horizon.
- Changing grass density, render scale, MSAA, particles, or shadow quality never changes vehicle physics, mowing coverage, or score results.

---

## 21. Development Milestones

### Milestone 1: Fuzzy planet visual proof

- Rust application using `winit`, `wgpu`, and WGSL
- Adapter capability inspection and baseline render path
- Static 15-meter-radius sphere
- Deterministic per-patch grass roots
- Opaque instanced tuft geometry
- Patch and horizon culling
- Near, middle, and horizon LODs
- Wind, simple grass lighting, and fuzzy silhouette
- Test swath that shortens blades to stubble
- Moving hover-wash source and damped-spring interaction field
- CPU and GPU timing overlay

**Exit condition:** The uncut sphere looks convincingly plush in motion, its entire visible limb remains fuzzy, a geometric shaved stripe reads clearly without relying on a painted ground texture, and grass bends and settles convincingly around a moving force source. Record timings on at least one machine in each provisional hardware class; both must meet their profile's 60 FPS target with useful headroom.

### Milestone 2: Spherical driving prototype

- Direct `rapier3d` integration with a dynamic vehicle and static sphere collider
- Four hover-pad queries and spring-damper forces
- Radial attraction, PD stabilization, propulsion, and lateral grip
- Fixed 120 Hz simulation with render interpolation
- Chase camera
- Full circumnavigation
- Manual and automatic recovery
- Grass bending from hover wash

**Exit condition:** Driving around and across the whole sphere feels stable for five uninterrupted minutes at multiple render frame rates, with no change to vehicle speed or handling.

### Milestone 3: Seeded planet generator

- Versioned random streams
- Seam-safe rolling terrain
- Mountain placement and displacement
- Grass/rock classification
- Starting-region selection
- Connectivity and clearance validation
- Seed entry, copy, replay, and regenerate flows

**Exit condition:** A 1,000-seed automated batch satisfies all generation invariants, and selected seeds are reproducible between clean launches.

### Milestone 4: Authoritative mowing and game loop

- Mowable surface classification
- CPU-authoritative 512×512-per-face mowing field
- Dirty-tile upload to the GPU mirror
- Seam-safe cut, comb-direction, and recent-cut fields
- Swept deck stamping
- Geometry-driven blade shortening and striping
- Coverage percentage
- Mountain collision accounting
- Timer, per-seed scoring, completion, and results
- Title, mode select, pause, and restart
- Persistent settings and best scores

**Exit condition:** A fresh launch supports a complete generated run and same-seed replay, and the player can reach 98% without seam artifacts or persistent false gaps.

### Milestone 5: Feel and presentation

- Final grass density, lighting, wind, clipping, and LOD tuning
- Final vehicle animation
- Camera polish
- Tutorial prompts
- Accessibility options
- Low, standard, and high visual presets

**Exit condition:** External playtesters describe mowing as satisfying and understand why their score changed.

### Milestone 6: Ship readiness

- Performance optimization
- Baseline and enhanced GPU-path verification
- Generator fuzzing and regression seeds
- Input-device edge cases
- Save migration/versioning
- Settings verification
- Long-session and restart testing
- Distribution build and credits

---

## 22. Test Matrix

At minimum, test:

- adapter selection, device creation, resize, fullscreen, suspend/resume, and recoverable surface errors on every supported operating system;
- baseline rendering with enhanced indirect-draw and experimental features disabled;
- capability-driven selection of enhanced submission where supported;
- equivalent visible results between baseline and enhanced grass paths;
- simulation equivalence at 30, 60, 120, and uncapped rendering rates;
- 120 Hz physics under low and standard visual presets;
- equality of generated-output hashes for repeated seed/version pairs;
- automated generation and validation of at least 1,000 arbitrary seeds;
- mountain centers adjacent to cube-face seams and arbitrary coordinate poles;
- minimum grass connectivity, corridor width, starting clearance, and weighted grass ratio;
- driving through arbitrary coordinate poles and every terrain-patch boundary;
- grass geometry viewed from near-ground, chase, look-behind, and high toy-camera angles;
- continuous fuzzy silhouette during a full camera orbit;
- stable stochastic LOD while accelerating, reversing, and changing camera height;
- mowing across every mowing-field and terrain-patch seam in both directions;
- maximum-speed mowing at low and unstable frame rates;
- holding ground albedo constant while verifying tall, cut, stripe, and boundary readability;
- hover-wash deformation across patch boundaries;
- hover-wash displacement and velocity across every cube-face boundary;
- grass spring recovery after stopping, reversing, boosting, and crossing an already-disturbed region;
- dirty mowing-tile uploads at cube corners and during maximum-speed diagonal passes;
- absence of gameplay dependence on the GPU mowing mirror;
- hovering on the steepest legal slope;
- collision recovery beside generated mountain concavities and narrow passes;
- entering and leaving the surface over a bump;
- switching input devices during a run;
- pausing during boost, collision, completion, and recovery;
- coverage near 98%, 99.5%, and 100%;
- resetting after a completed and an incomplete run;
- replaying the same seed and generating a different one;
- color-independent readability of cut grass and exposed rock; and
- camera behavior while circling the base of the tallest generated mountains.

---

## 23. Principal Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Custom runtime grows into an engine project | Keep the architecture game-specific, use focused foundation crates, require a demonstrated game need before adding generalized systems |
| GPU backend capabilities diverge | Maintain a conservative baseline path, inspect capabilities at startup, and treat indirect draws and mesh shaders as optional optimization tiers |
| Dependency upgrades destabilize performance | Pin the lockfile, upgrade deliberately, and run visual, generator, physics, and frame-time regression suites before accepting changes |
| Spherical driving causes disorientation | Strong local-up camera, visible landmarks, adjustable follow behavior, fixed-horizon accessibility option |
| Coverage seams create impossible final patches | Cube-map representation, swept stamps, explicit seam tests, 98% completion tolerance |
| Dense geometric grass is too expensive | Surface-patch and horizon culling, instancing or indirect draws, stable stochastic thinning, measured visible-tuft budget |
| Thin blades shimmer or disappear | Opaque tapered geometry, minimum projected width, stable thinning, modest LOD widening, and 2×/4× MSAA; add TAA only after validating animated motion |
| LOD destroys the fuzzy silhouette | Preserve blade height at the limb, thin rather than flatten, and keep real geometry in every ordinary gameplay view |
| Grass interaction field rings, explodes, or reveals seams | Clamp energy, use a stable damped integrator, store world-space tangent vectors, test cube boundaries, and provide a storage-buffer fallback |
| Low-spec fill rate misses 60 FPS | Scale tuft density, internal resolution, MSAA, particles, and optional shadows while preserving 120 Hz physics and mowing state |
| Generated terrain contains inaccessible grass | Connectivity and clearance validation, deterministic repair, reclassification of tiny islands, deterministic regeneration fallback |
| Generated planets feel interchangeable | Correlated regional variation, distinct mountain silhouettes, constrained parameter families, seed replay and favorites |
| Hover physics feels floaty and imprecise | Strong lateral grip at low speed, damped orientation, limited air time, data-driven tuning |
| Cleanup at 95% becomes tedious | Completion tolerance and locator for the largest nearby uncut region |
| Seed difficulty makes records incomparable | Store records per seed; reserve cross-player competition for curated or daily shared seeds |
| Cute presentation obscures mower width or hazards | Strong deck silhouette, ground projection, contrast, and multimodal feedback |

---

## 24. Post-MVP Expansion

Expansion should deepen the central activity rather than replace it.

Potential additions:

- additional generator biomes, mountain archetypes, planet radii, grass species, and weather;
- curated and daily shared seeds with ghost routes and leaderboards;
- ring-shaped paths or small moons connected by temporary launch arcs;
- different mower decks and hover handling profiles;
- lawns that must be cut into requested patterns;
- rain that changes grip and makes clippings clump;
- wandering gentle creatures the player must avoid;
- co-op mowing on the same planet;
- photo mode centered on the final lawn pattern; and
- a lightweight progression map of increasingly eccentric gardening jobs.

Any expansion should preserve complete free traversal, persistent visible mowing, and short self-contained sessions.

---

## 25. Decisions Deferred Until Playtesting

The following should remain tunable rather than settled on paper:

- exact planet radius and maximum speed;
- final minimum GPU, required internal resolution, and acceptable dynamic-resolution range;
- grass-root density, tuft topology, blade dimensions, and LOD distances;
- interaction-field resolution, storage-texture format, spring constants, and storage-buffer fallback representation;
- the measured threshold for enabling GPU compaction and indirect draws;
- whether MSAA alone is sufficient or a motion-vector-aware temporal solution is required;
- precise mountain count, angular spacing, height, footprint, and rock ratio;
- cube-sphere versus subdivided-icosphere terrain patches;
- whether arbitrary seeds are always exposed or primarily selected through a curated flow;
- whether the mower is front-mounted or centered beneath the car;
- strength of drift and air control;
- whether boost temporarily reduces cutting effectiveness;
- exact completion and rating thresholds;
- the need for a fixed-horizon camera mode; and
- whether results playback is worth its storage and implementation cost.

The first decisive prototype question is: **Does real geometric grass make the tiny sphere look irresistibly plush at the horizon, and does shaving a clean path through it feel delightful?** All further systems depend on that answer.
