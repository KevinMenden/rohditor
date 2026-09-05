Yes — I think dedicated crates are the right direction, especially if we want the algorithms to stay independently testable and not get entangled with Rohditor’s pipeline/UI.

The main design choice I’d make is **not one giant `highlight-reconstruction` crate containing everything**, but a small set of crates grouped by processing domain. The reason is that RAW highlight reconstruction, demosaiced-RGB reconstruction, and output gamut mapping have fundamentally different inputs and constraints.

## What RawTherapee does

RawTherapee exposes several highlight-reconstruction modes. Its documentation describes four traditional families: luminance recovery, CIELab reconstruction, color propagation, and blend. Color propagation is described as the strongest general-purpose option: it restores luminosity and spreads color information from valid neighboring regions into clipped regions, but it can bleed incorrect colors and is relatively expensive. ([rawpedia.rawtherapee.com][1])

The current source is more interesting than the older UI terminology. RawTherapee has dedicated highlight-reconstruction code in `rtengine/hilite_recon.cc`, and its RAW image path distinguishes color-propagation-style recovery from other highlight-recovery methods. ([GitHub][2]) The implementation includes `HLRecovery_inpaint(...)` and a newer `highlight_recovery_opposed(...)` path.

That source file is GPLv3-or-later and explicitly contains substantial dedicated image-processing infrastructure: box blurs, guided filtering, resampling and related operations.  So this isn't just a three-line channel substitution — their more advanced reconstruction is spatial and multi-stage.

One notable detail is that RawTherapee's color reconstruction can happen before its regular white-balance multiplication in that processing path. The source calls either color propagation or opposed reconstruction, and only afterward applies the white-balance multipliers. ([GitHub][2]) That's conceptually attractive because it tries to repair sensor-derived color relationships before WB amplifies clipping imbalance.

RawTherapee itself is GPL-3.0+, which is compatible with Rohditor now being GPLv3+. ([GitHub][3])

---

# What darktable does

Darktable currently has a broader and, architecturally, cleaner set of explicit reconstruction algorithms.

Its source enum currently includes:

```text
Clip highlights
Reconstruct in LCh
Reconstruct color / inpaint
Guided laplacians
Segmentation based
Inpaint opposed
```

Those correspond directly to modes in `src/iop/highlights.c`.

The important ones for Rohditor are these.

### 1. Clip highlights

This is the simplest baseline.

If one CFA channel clips first, bring the other channels down so everything reaches the same clipping limit.

Conceptually:

```text
R = 1.00 clipped
G = 0.82
B = 0.61

→

R = 1.00
G ≈ reconstructed neutral relationship
B ≈ reconstructed neutral relationship
```

Or even more conservatively, force the highlight toward achromatic.

Its advantages are robustness, trivial implementation and no weird color bleeding. Its disadvantage is obvious: you lose potentially useful surviving color/detail information.

Darktable recommends it mainly for things that should naturally be desaturated, like clouds. ([darktable][4])

I absolutely would implement this in Rohditor because it gives us a **reference implementation**.

---

### 2. Local channel/color inpainting

Darktable's current default is called **inpaint opposed**. It estimates the missing color using nearby valid pixels. ([darktable][4])

This is conceptually close to what I suggested previously as the best first practical algorithm.

Suppose:

```text
clipped pixel:
R = clipped
G = 0.72
B = 0.35
```

and nearby valid pixels suggest:

```text
R/G ≈ 1.35
B/G ≈ 0.48
```

Then you can infer something like:

```text
R ≈ 0.72 × 1.35
B ≈ 0.72 × 0.48
```

The actual algorithms are more careful than that, particularly around transitions, but **local color ratios** are the basic idea.

This class of reconstruction has a very attractive cost/quality ratio.

It is:

* local,
* deterministic,
* fairly inexpensive,
* suitable before demosaicing,
* relatively straightforward to SIMD/parallelize,
* much easier to understand and test than segmentation or Laplacians.

This should probably be Rohditor's first real reconstruction algorithm.

---

### 3. LCh reconstruction

Darktable analyzes clipped sensor blocks and performs reconstruction using LCh-like color relationships. It still tends toward monochrome highlights, but preserves more luminosity/detail than simple clipping. ([darktable][4])

This is interesting historically and as a fallback, but I would **not prioritize it for Rohditor**.

Why?

Because introducing perceptual color-space logic into a RAW-domain reconstruction algorithm makes the abstraction murkier. And better spatial methods now exist.

It could still be useful later for compatibility/research.

---

### 4. Segmentation-based reconstruction

This one is much more interesting.

Darktable identifies contiguous clipped areas as **segments** rather than treating each pixel independently.

For each segment it studies valid pixels around its boundary and tries to infer the segment's color from them. Darktable rejects poor candidate pixels such as very dark pixels and edges; if it can't get a reliable estimate, it falls back to simpler inpainting. ([darktable][4])

The source implementation actually describes its processing stages. Among them, it builds gradient information and performs segmentation of fully clipped data.

This solves a major weakness of simple local inpainting.

Imagine:

```text
        red flower
      █████████████
    ██ clipped area ██
      █████████████
             green leaf
```

A local blur/inpainting algorithm might pull green from the leaf into the flower.

Segmentation lets us reason:

> "This entire blown patch is probably one object. Use the boundary evidence belonging to that region rather than blindly averaging everything nearby."

That's a significant quality improvement.

The downside is complexity:

* connected-component labeling,
* boundary extraction,
* candidate filtering,
* region merging,
* gradients,
* fallback logic,
* more memory,
* harder parallelization.

I would consider this a **second-generation Rohditor algorithm**.

---

# 5. Guided Laplacian reconstruction

This is probably the most technically interesting one.

Darktable puts the implementation in a separate source area:

```text
src/iop/hlreconstruct/laplacian.c
```

rather than stuffing it directly into the normal module.

That alone is quite similar to the architecture you're proposing.

The core idea is not merely:

> blur nearby color into the missing area.

Instead, it operates at multiple spatial scales and tries to reconstruct the **structure of the image**.

Very roughly:

```text
image
 ↓
low-frequency component
 +
high-frequency / Laplacian detail
 ↓
reconstruct each scale using valid channels as guides
 ↓
combine
```

Darktable's code explicitly fits the Laplacian of chromaticity against the Laplacian of image norm/intensity — essentially guiding missing chromatic detail from structure present in valid information.

That lets it reproduce things such as:

```text
bright lamp
metal reflection
specular highlight
hair crossing a blown region
small textured clipped area
```

much better than simple color propagation.

Darktable's documentation particularly recommends guided Laplacians for smoothing clipped-area boundaries, recovering spotlights/small-to-medium clipped regions, and avoiding demosaic-induced chromatic artifacts near clipped boundaries. ([darktable][4])

But it is computationally expensive and multi-scale.

So for Rohditor:

**very desirable eventually, but not the first algorithm I'd build.**

---

# 6. An important clue: RawTherapee and darktable are converging somewhat

RawTherapee historically talks mostly about color propagation / blend / Lab recovery.

But the newer RawTherapee source has `highlight_recovery_opposed`, and darktable now also has an **inpaint opposed** mode.

That is worth investigating further when implementing, because this family seems to represent a relatively modern sweet spot:

```text
much better than naive clip
less complicated than segmentation
far cheaper than guided Laplacians
```

I would likely use this as one of the main references for Rohditor's initial high-quality algorithm.

---

# 7. I would structure the Rohditor crates like this

Rather than:

```text
rohditor-highlight-reconstruction
```

containing everything, I'd use something closer to:

```text
crates/
    highlight/
    gamut/
    tone-map/
```

or slightly more explicit:

```text
rohditor-highlight
rohditor-gamut
rohditor-tonemap
```

Your current repository already separates `raw`, `demosaic`, `core`, `image`, `edit`, and `gpu`, so this follows the existing architecture nicely.

For highlights, I would make the crate **independent of `core`**.

Something like:

```text
rohditor-highlight
    ├── clip.rs
    ├── opposed.rs
    ├── ratios.rs
    ├── segmentation.rs
    ├── laplacian.rs
    └── mask.rs
```

with a public API conceptually resembling:

```rust
pub enum HighlightMethod {
    Clip,
    LocalRatios,
    Opposed,
    Segmentation,
    GuidedLaplacian,
}
```

But I would go one step further than that.

---

# 8. Don't make the algorithms depend directly on `RawFrame`

That would create unnecessary coupling.

Instead, have the highlight crate consume the smallest useful representation.

For example:

```rust
pub struct RawHighlightInput<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub cfa: BayerPattern,
    pub clip_levels: [f32; 3],
}
```

or, since Rohditor has already normalized per-photosite black/white levels:

```rust
pub struct NormalizedMosaicView<'a> {
    ...
}
```

Then:

```rust
pub trait HighlightReconstructor {
    fn reconstruct(
        &self,
        input: &NormalizedMosaicView,
        output: &mut NormalizedMosaic,
        options: &HighlightOptions,
    ) -> Result<HighlightStats, HighlightError>;
}
```

That makes the crate usable outside Rohditor's main pipeline.

It also means its tests can create tiny synthetic Bayer images directly.

---

# 9. Clipping detection should probably be its own reusable primitive

This matters more than it first appears.

You don't want each reconstruction implementation independently deciding:

```text
is this photosite clipped?
```

because then algorithms won't be comparable.

I'd make one shared mask generator:

```rust
pub struct ClippingMask {
    ...
}

pub fn detect_clipping(
    mosaic: &NormalizedMosaicView,
    threshold: f32,
) -> ClippingMask;
```

And distinguish at least:

```text
not clipped
near clipped
clipped
```

rather than only a boolean.

Something like:

```text
valid       < 0.98
transition  0.98..1.0
clipped     >= 1.0
```

with an adjustable tolerance.

The exact threshold deserves more thought because RAW white levels aren't magical perfect saturation boundaries and some cameras preserve headroom above nominal white.

Rohditor already deliberately keeps values such as `1.3` after normalization, so we should not naïvely say:

```rust
value >= 1.0 == destroyed
```

for every camera.

That's one area where the reconstruction crate may need a small amount of camera/sensor context supplied by the RAW layer.

---

# 10. I'd separate detection from reconstruction

The pipeline becomes:

```text
normalized Bayer
       │
       ▼
┌────────────────────┐
│ clipping detector  │
└────────────────────┘
       │
       ├── clipping mask
       │
       ▼
┌────────────────────┐
│ reconstruction     │
└────────────────────┘
       │
       ▼
reconstructed Bayer
       │
       ▼
demosaic
```

That is valuable because the mask itself can later power UI features:

```text
show clipped RAW pixels
show unrecoverable areas
diagnostic overlay
auto-select algorithm
statistics
```

---

# 11. Tests become exceptionally useful here

This crate would be perfect for synthetic tests.

Example:

### One clipped red photosite

```text
true underlying RGB:
R = 1.2
G = 0.7
B = 0.3

sensor:
R = clipped
G valid
B valid
```

Check whether the reconstructed R is sensible.

### Flat colored surface

A large uniform red region where some pixels clip.

Expected result:

```text
no hue discontinuity at clipping boundary
```

### Edge between red and green objects

```text
RED RED RED | GREEN GREEN
RED CLIP    | GREEN GREEN
RED RED RED | GREEN GREEN
```

Test that color from the green side doesn't bleed badly into the red clipped area.

### Fully clipped area

```text
all channels gone
```

The algorithm shouldn't claim miraculous recovery; it should degrade predictably.

### Specular highlight

Expected behavior:

```text
chromatic object → smoothly approaches neutral white
```

These synthetic fixtures would make comparisons between algorithms much easier than visually eyeballing random photos.

---

# 12. A useful output from every algorithm

Rather than returning pixels only, I'd consider:

```rust
pub struct ReconstructionStats {
    pub clipped_sites: usize,
    pub reconstructed_sites: usize,
    pub fallback_sites: usize,
    pub fully_clipped_sites: usize,
}
```

Potentially later:

```rust
pub enum ReconstructionConfidence {
    High,
    Medium,
    Low,
}
```

per region.

That sounds like unnecessary machinery now, but segmentation algorithms already have an implicit concept of confidence because they reject bad reconstruction candidates. Darktable explicitly falls back when surrounding candidates are unsuitable. ([darktable][4])

Making that explicit could become a nice Rohditor advantage.

---

# 13. What I'd actually implement first

I'd do the algorithms in this order:

| Phase | Algorithm                       | Why                                  |
| ----- | ------------------------------- | ------------------------------------ |
| 1     | **Clip / neutralize**           | baseline and debug reference         |
| 2     | **Local ratio reconstruction**  | simple, teaches us the problem       |
| 3     | **Opposed / local inpainting**  | likely best first production default |
| 4     | **Segmentation reconstruction** | solves larger regions/color bleeding |
| 5     | **Guided Laplacian**            | maximum-quality option               |

I would probably skip LCh initially.

Not because it is useless, but because it doesn't teach us much architecturally that the others don't already cover.

---

# 14. Then a separate gamut crate

I would **not** put gamut compression in the highlight crate.

Instead:

```text
crates/gamut/
```

with something like:

```rust
pub enum GamutMapper {
    Clip,
    ChromaCompress,
    AcesLike,
}
```

That crate would operate on:

```text
LinearRgbImage<f32>
```

rather than a Bayer mosaic.

Eventually it might expose two uses:

```text
camera → Rec2020 working-gamut compression
Rec2020 → output-sRGB gamut compression
```

possibly sharing the same mathematical primitives.

This separation is important:

```text
highlight crate
    understands CFA / clipped sensor samples

gamut crate
    understands RGB color spaces / chromaticity
```

Neither needs to know about the application's UI or export logic.

---

# 15. And later a tone-mapping crate

Likewise:

```text
crates/tonemap/
```

for:

```text
Scene-linear HDR
        ↓
display-referred range
```

Candidates there could eventually include:

```text
simple shoulder
sigmoid
filmic
AgX-style
```

Again, this has nothing intrinsically to do with missing RAW samples.

So eventually the clean Rohditor dependency structure could look approximately like:

```text
raw
 │
 ▼
highlight
 │
 ▼
demosaic
 │
 ▼
core color conversion
 │
 ▼
edit
 │
 ├──────────────┐
 ▼              ▼
tonemap        gamut
 │              │
 └──────┬───────┘
        ▼
      output
```

Or `core` orchestrates all of them, while those processing crates remain independently usable.

---

# 16. GPL makes studying these implementations especially useful now

This is exactly the kind of benefit you anticipated when choosing GPLv3+.

RawTherapee's relevant highlight source explicitly says GPLv3-or-later.  Darktable itself reports GPLv3-or-later too. ([GitHub][5])

So Rohditor can do more than vaguely reimplement ideas from papers.

We can study the actual battle-tested implementations, port/adapt algorithms where appropriate, retain attribution and copyright notices where code is derived, and redesign them into safe Rust interfaces.

I would **not mechanically translate C/C++ line-by-line**, though.

For something like Darktable's guided Laplacian implementation, I'd use its code to understand:

```text
algorithm
numerical assumptions
edge handling
thresholding
multi-scale structure
performance tricks
```

and then implement the architecture naturally in Rust.

That will produce much better Rohditor code.

---

## The architecture I'd recommend

I'd start with a new crate named something like **`rohditor-highlight`** whose sole domain is RAW/CFA highlight reconstruction.

Its first version would contain:

```text
mask/detection
clip baseline
local-ratio reconstruction
opposed/inpainting reconstruction
common neighborhood/CFA utilities
synthetic quality tests + benchmarks
```

and reserve clean module/API boundaries for:

```text
segmentation
guided Laplacian
```

later.

Then `core::pipeline` would acquire one explicit stage:

```text
normalize
→ highlight reconstruction
→ demosaic
```

instead of knowing anything about how reconstruction actually works.

That's probably the cleanest way to extend the architecture you already have.

The next useful step is to inspect **RawTherapee's `HLRecovery_inpaint` / `highlight_recovery_opposed` and darktable's `inpaint opposed` implementations in detail**, translate the math into pseudocode, and compare them side-by-side. From that we can choose exactly what `rohditor-highlight` v1 should implement rather than inventing our own algorithm prematurely.

[1]: https://rawpedia.rawtherapee.com/RawPedia.pdf?utm_source=chatgpt.com "10/25/2020
RawPedia Book - RawPedia
Snapshot from"
[2]: https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/rawimagesource.cc?utm_source=chatgpt.com "RawTherapee/rtengine/rawimagesource.cc at dev · RawTherapee/RawTherapee · GitHub"
[3]: https://github.com/RawTherapee/RawTherapee?utm_source=chatgpt.com "GitHub - RawTherapee/RawTherapee: A powerful cross-platform raw photo processing program · GitHub"
[4]: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/highlight-reconstruction/?utm_source=chatgpt.com "darktable user manual - highlight reconstruction"
[5]: https://github.com/darktable-org/darktable/blob/master/src/common/darktable.c?utm_source=chatgpt.com "darktable/src/common/darktable.c at master · darktable-org/darktable · GitHub"
