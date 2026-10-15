#!/usr/bin/env bash
set -euo pipefail

assert_no_dependencies() {
    local package="$1"
    local forbidden="$2"
    if cargo tree --locked --edges normal --prefix none --package "$package" \
        | grep -Eq "^(${forbidden}) "; then
        echo "$package has an upward dependency on one of: $forbidden" >&2
        return 1
    fi
}

assert_no_dependencies rohditor-image 'rohditor-(core|demosaic|edit|gpu|raw|cli|desktop)'
assert_no_dependencies rohditor-edit 'rohditor-(core|demosaic|gpu|raw|cli|desktop)'
assert_no_dependencies rohditor-demosaic 'rohditor-(core|edit|gpu|raw|cli|desktop)'
assert_no_dependencies rohditor-highlight 'rohditor-(core|demosaic|edit|gpu|raw|cli|desktop)'
assert_no_dependencies rohditor-optics 'rohditor-(core|gpu|cli|desktop)'
assert_no_dependencies rohditor-raw 'rohditor-(core|demosaic|edit|gpu|cli|desktop)'
assert_no_dependencies rohditor-color 'rohditor-(core|gpu|cli|desktop)'
assert_no_dependencies rohditor-core 'rohditor-(gpu|cli|desktop)'
assert_no_dependencies rohditor-gpu 'rohditor-(cli|desktop)'
# Headless GPU export is an application-level dependency; core stays GPU-free.
assert_no_dependencies rohditor-cli 'rohditor-desktop'

# The reference harness is dev-only: no other crate may depend on it.
if grep -rl 'rohditor-reference' --include=Cargo.toml apps crates \
    | grep -qv '^crates/reference/'; then
    echo "a crate depends on the dev-only rohditor-reference harness" >&2
    exit 1
fi

if grep -Eq 'pub use rohditor_(demosaic|edit|image)' \
    crates/core/src/lib.rs crates/raw/src/lib.rs; then
    echo "found a legacy facade re-export for an extracted crate" >&2
    exit 1
fi

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
