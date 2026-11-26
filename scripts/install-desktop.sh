#!/usr/bin/env bash
# Install a release build of the desktop app (GPUI, Wayland only) and its
# Freedesktop metadata below a user-controlled prefix. The default is ~/.local,
# so no root access is required.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
project_root=$(cd -- "$script_dir/.." && pwd)
install_prefix=${1:-"${HOME}/.local"}
desktop_id=io.github.kevin.rohditor
binary_path=${ROHDITOR_BINARY:-"$project_root/target/release/rohditor-desktop"}

if [[ ! -x "$binary_path" ]]; then
    printf 'Expected an executable release binary at %s\n' "$binary_path" >&2
    printf 'Build one first: cargo build --release --locked -p rohditor-desktop\n' >&2
    exit 1
fi

install -Dm755 "$binary_path" "$install_prefix/bin/rohditor-desktop"
# The neural demosaic's WebGPU provider loads this through the binary's rpath
# (.cargo/config.toml); without it, the model runs on the CPU only.
dawn_library="$(dirname -- "$binary_path")/libwebgpu_dawn.so"
if [[ -e "$dawn_library" ]]; then
    install -Dm755 "$dawn_library" "$install_prefix/lib/rohditor/libwebgpu_dawn.so"
else
    printf 'Warning: %s not found; the neural demosaic will run on the CPU only.\n' "$dawn_library" >&2
fi
install -Dm644 "$project_root/assets/$desktop_id.desktop" \
    "$install_prefix/share/applications/$desktop_id.desktop"
install -Dm644 "$project_root/assets/$desktop_id.svg" \
    "$install_prefix/share/icons/hicolor/scalable/apps/$desktop_id.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$install_prefix/share/applications"
fi

printf 'Installed Rohditor below %s\n' "$install_prefix"
printf 'Ensure %s/bin is on PATH before launching from the desktop menu.\n' "$install_prefix"
