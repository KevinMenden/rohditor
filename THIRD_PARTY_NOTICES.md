# Third-party notices

## lensfun-rs 0.7.0

Rohditor uses the pure-Rust `lensfun` crate, version 0.7.0, for Lensfun
profile matching and calibration models. The crate's code is licensed under
the GNU Lesser General Public License, version 3.0 or later. The bundled
Lensfun XML database is licensed under the Creative Commons Attribution-
ShareAlike 3.0 Unported license.

The crate and its bundled-data attribution are available at
<https://github.com/vdavid/lensfun-rs>. The complete license texts and the
upstream contributor notices are included in the dependency's `NOTICE`,
`LICENSE-LGPL-3.0`, and `LICENSE-GPL-3.0` files.

## ONNX Runtime 1.31 (via `ort` 2.0.0-rc.15)

The neural demosaic runs on Microsoft's ONNX Runtime, which the `ort` crate
downloads as a prebuilt library at build time and links into the Rohditor
binaries. ONNX Runtime is licensed under the MIT License, Copyright (c)
Microsoft Corporation; `ort` is licensed under MIT OR Apache-2.0. Sources and
license texts are available at <https://github.com/microsoft/onnxruntime> and
<https://github.com/pykeio/ort>.

GPU inference uses ONNX Runtime's WebGPU provider, built on Google's Dawn,
which is distributed next to the binaries as `libwebgpu_dawn.so`. Dawn is
licensed under the BSD 3-Clause License, Copyright 2017-2023 The Dawn & Tint
Authors; see <https://dawn.googlesource.com/dawn>.

The neural demosaic model itself is not part of the binaries. It is installed
separately (`rohditor-cli model install <FILE>`); its weights license and the
attribution of its training data are recorded in the model's `model.json`.

## GPUI

The desktop app (`apps/desktop`) uses GPUI from Zed Industries, licensed under
the Apache License, Version 2.0; see <https://github.com/zed-industries/zed>.
`apps/desktop/vendor/gpui_wgpu` is a modified copy of its wgpu renderer. The
changes are marked `ROHDITOR`, and the license text is included there as
`LICENSE-APACHE`.
