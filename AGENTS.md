# Project guidelines

Focus on good maintainabiliy when writing code. Create crates where necessary. 
Make modules if that's a good fit. Split files where they get too long.
Write multiple functions where it makes sense to split up a bigger one.
Make things modular where this can benefit in the long term.
Add comments in the code where it could be helpful.

The goal of this project is to slowly and deliberately create a nice RAW-editor. It will stay focused on the basic functionality at first, but the plan is to build it out with a lot of features. Therefore, we need to think about code structure when adding things.
Keep things modular, so they are understandable when regarding on their own: create crates where plausible, create extra files where there is clearly bundled functionality, create folders/subfolder where apprpriate. Create abstractions where it makes sense. The goal is to make it easier to understand a specific part of the codebase, without having to look at the entire codebase, which will eventually be large.


-  Lower layers must not depend on the desktop UI. We want to be able to essentialy replace the UI if we wish to do so, still using the processing functionalities
   of Rohditor
- Keep RAW data immutable. Represent edits as a validated, versioned recipe
- Preserve typed image states (sensor mosaic, linear RGB, display RGB), checked
  dimension/byte arithmetic, cancellation, and transactional output writes.
- Run `./scripts/check.sh` before handing off code. Use the ignored private and
  GPU suites when changing decoder, full-resolution, or GPU behavior:
  `cargo test --release --workspace --tests -- --ignored --nocapture` and
  `cargo test --release -p rohditor-gpu -- --ignored --nocapture`.
- Benchmark image-processing changes on representative dimensions and add
  small asymmetric correctness tests. Keep UI rendering performance separate
  from image-processing performance.
- Don't create extra documentation unless you are asked to do so.
- This is currently pre-release and not stable. We don't need to be backwards compatible in anyway. Breaking changes and API changes are okay and the better option.

- Think Before Coding
  Don't assume. Don't hide confusion. Surface tradeoffs.
  Before implementing:
  State your assumptions explicitly. If uncertain, ask.
  If multiple interpretations exist, present them - don't pick silently.
  If a simpler approach exists, say so. Push back when warranted.
  If something is unclear, stop. Name what's confusing. Ask.
- Simplicity First
  Minimum code that solves the problem. Nothing speculative.
      No features beyond what was asked.
      No abstractions for single-use code.
      No "flexibility" or "configurability" that wasn't requested.
      No error handling for impossible scenarios.
      If you write 200 lines and it could be 50, rewrite it.
  Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.
