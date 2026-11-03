---
name: rohditor-review
description: Review a pull request or diff in the rohditor RAW editor against the project's architecture rules and image-processing correctness concerns. Use when asked to review code, a PR, or a branch in this repo.
---

# Rohditor code review

Review the change as a senior reviewer who knows this codebase. Read `AGENTS.md` first;
it is the source of truth for layering and rules. Get the diff with `gh pr diff <n>`
(or `git diff main...HEAD` locally) and read surrounding code when the diff alone is
not enough to judge a change.

## What to check, in priority order

1. **Correctness**
   - Dimension/byte arithmetic: unchecked `*`/`+` on width, height, stride, channel
     counts; `usize`/`u32` casts that can truncate or overflow; off-by-one at image edges.
   - Typed image states: sensor mosaic vs linear RGB vs display RGB must not be mixed.
     Look for gamma/transform applied twice or in the wrong space, wrong CFA phase or
     crop offset, premultiplied vs straight data.
   - CPU vs GPU parity: a change to one path should be reflected in (or tested against)
     the other (`crates/core` vs `crates/gpu`).
   - Cancellation is honoured in long loops; output files are written transactionally
     (temp file + rename), never partially.
   - Panics on user data (`unwrap`, indexing, slicing) in decoder, catalog, or sidecar code.
2. **Architecture** (see "Architecture" and "Rules" in `AGENTS.md`)
   - A lower crate depending on `core`, `gpu`, `engine`, or an app.
   - `eframe`/`egui`/`winit`/`rfd` used under `crates/`.
   - `rohditor-engine` re-exporting lower crates (`pub use rohditor_*`).
   - Editing math or scheduling/caching/undo/persistence placed in `apps/*` instead of
     the lower crates or the engine.
   - Editing math that is not a pure function over `EditRecipe`.
3. **Data model**
   - RAW data stays immutable; edits go through the validated, versioned recipe.
   - Recipe changes validate their inputs and keep serialization/versioning coherent.
4. **Tests**
   - Image-processing changes need small asymmetric correctness tests (non-square,
     non-symmetric input, so transposed/flipped bugs show up).
   - Performance-sensitive changes should mention representative-size benchmarks.
   - Flag logic that is hard to test as written.
5. **Maintainability**
   - Overlong files or functions that should be split; a clear module/crate home.
   - Abstractions for single-use code, speculative flexibility, dead code.
   - Comments missing where the reasoning is non-obvious.

## What not to do

- Don't comment on formatting or lints; `./scripts/check.sh` (rustfmt, clippy, tests,
  dependency-direction checks) already runs in CI.
- Don't suggest backwards-compatibility shims; the project is pre-release and breaking
  changes are preferred.
- Don't ask for extra documentation files.
- Don't pad the review. If the change is fine, say so briefly.

## Output

- Leave inline comments only for concrete issues, each with: what is wrong, why it
  matters (a failing input or scenario where possible), and a suggested fix.
- Prefix severity: **bug**, **arch**, **test**, or **nit**. Keep nits to a minimum.
- Finish with one short summary comment: overall verdict and the 1-3 most important items.
