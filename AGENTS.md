# Project guidelines

Keep Rohditor small, readable, and focused on the current Sony ILCE-6400
workflow.

- Keep boundaries clear: `raw` decodes files, `core` owns image processing,
  `gpu` owns interactive GPU work, and `apps/` owns CLI/UI concerns. Lower
  layers must not depend on the desktop UI.
- The deterministic CPU pipeline is the correctness reference. GPU processing
  is optional interactive preview work and must retain a CPU fallback.
- Keep RAW data immutable. Represent edits as a validated, versioned recipe;
  background jobs use document/revision IDs and discard stale results.
- Preserve typed image states (sensor mosaic, linear RGB, display RGB), checked
  dimension/byte arithmetic, cancellation, and transactional output writes.
- Keep the current scope explicit: Linux desktop, Sony ILCE-6400 ARW files,
  sRGB JPEG/PNG output. Do not add general-purpose architecture until a real
  feature requires it.
- Run `./scripts/check.sh` before handing off code. Use the ignored private and
  GPU suites when changing decoder, full-resolution, or GPU behavior:
  `cargo test --release --workspace --tests -- --ignored --nocapture` and
  `cargo test --release -p rohditor-gpu -- --ignored --nocapture`.
- Benchmark image-processing changes on representative dimensions and add
  small asymmetric correctness tests. Keep UI rendering performance separate
  from image-processing performance.
- Keep documentation limited to this file, `docs/status.md`, and a concise
  README. Update checkboxes and commands when behavior changes; do not create
  phase or subsystem documentation pages unless explicitly needed.
