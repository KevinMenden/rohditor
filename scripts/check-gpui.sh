#!/usr/bin/env bash
# The gate for apps/gpui, which is a Cargo workspace of its own.
set -euo pipefail
cd "$(dirname "$0")/../apps/gpui"

# Without --all and with --no-deps, so the vendored GPUI code keeps its upstream style.
cargo fmt --check
cargo clippy --no-deps --all-targets --locked -- -D warnings
cargo test --locked
