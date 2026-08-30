# Phase 5 GPU preview path

The GPU processor accelerates only the interactive downstream preview. RAW
decode, normalization, white balance, demosaic, camera color conversion, and
full-resolution export remain owned by the deterministic CPU reference path.
This keeps the GPU shader small, gives it a stable numeric contract, and leaves
the editor usable without a hardware GPU.

## Boundary and ownership

`CpuPipeline::prepare_preview_base` produces a `DemosaicedBase`: linear
Rec.2020/D65 data after the upstream RAW stages and before exposure, contrast,
saturation, orientation, and sRGB encoding. The desktop worker creates this
base off the UI thread when a document opens or its white balance changes.

Before sending the result, the worker packs that base into a `GpuPreviewUpload`
containing RGBA16Float texels; this conversion also stays off the UI thread.
The UI thread submits that payload once to the shared eframe `wgpu` device. The
active document's GPU preview state owns the resulting source texture for
downstream edits. Exposure,
contrast, saturation, and orientation changes dispatch the shader directly
against that resident source; they do not re-decode, re-demosaic, re-pack, or
re-upload the unchanged base.

The worker never owns `wgpu` objects. Conversely, the UI thread never decodes
or demosaics RAW data. This division avoids a second device, keeps renderer
ordering straightforward, and preserves the Phase 4 background-work contract.

## Textures and display

For one active document preview, GPU state retains:

- an `Rgba16Float` upload texture containing the linear Rec.2020 base;
- an `Rgba16Float` linear working texture for future GPU stages; and
- an `Rgba8Unorm` transfer-encoded sRGB display texture.

One WGSL compute dispatch evaluates exposure, contrast, saturation, physical
EXIF orientation, Rec.2020-to-sRGB conversion, hard clip, and the sRGB transfer
function. It writes the linear working and display textures in the same pass.
The display texture is registered directly with `egui_wgpu::Renderer`, so the
viewport samples it natively. Normal desktop rendering never reads that texture
back to CPU memory.

The explicit `readback_display` API exists only for tests and diagnostics. It
copies the display texture to a padded map-read buffer and must not be used in
the viewport path.

When dimensions and orientation are unchanged, later edits reuse both working
and display textures as well as the source texture. Phase 6 records whether
that reuse occurred and estimates retained texture bytes. It also attaches a
shared-queue completion callback to each dispatch. The callback is non-blocking
in the desktop; only controlled benchmark tests use the explicit queue wait.

## Device selection and fallback

The application receives the adapter, device, queue, renderer, and target
format from `eframe::CreationContext::wgpu_render_state`; it never creates a
second adapter or `wgpu::Device`. At startup it records adapter name, backend,
type, driver information, target format, maximum texture/workgroup limits,
required float/unorm texture usages, and timestamp-query availability.

`--processor auto` selects a compatible non-CPU adapter and falls back to the
CPU processor with a visible reason if initialization or GPU preview work
fails. `--processor gpu` requires that path and reports an error instead of
silently changing processors. `--processor cpu` does not create Rohditor GPU
processing resources. A `glow` renderer has no shared eframe `wgpu` state, so
it uses the same Auto fallback behavior.

## Numeric contract

The shader mirrors the CPU operation order:

1. multiply by `2^EV`;
2. apply contrast around 18% linear gray with slope `2^contrast`;
3. adjust saturation around Rec.2020 luminance;
4. map oriented output coordinates to the retained source base;
5. transform linear Rec.2020 to linear sRGB, hard clip to `[0, 1]`, and encode
   sRGB.

The retained source and working textures use half-float storage, so GPU output
is not required to be bit-identical to CPU `f32` output. The current acceptance
tolerance is at most two 8-bit sRGB code values per channel.

## Verification

The normal unit suite checks parameter layout, orientation metadata handoff,
and padded source upload handling. The opt-in synthetic Vulkan check covers all
eight EXIF orientations. The following opt-in checks require Vulkan:

```console
cargo test -p rohditor-gpu -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

The first uses a small synthetic RAW fixture across all eight EXIF
orientations. The second command also runs the private `DSC00851.ARW` parity
test when the local corpus is present.
If Vulkan exposes only a CPU rasterizer, the opt-in GPU tests report that they
were skipped rather than treating software rendering as GPU validation.

On the reference eframe device, timestamp queries are unavailable. The
developer diagnostics therefore report encode/submit CPU time and conservative
queue-completion wall latency, not an isolated shader timestamp. Current
measurements are recorded in [`preview-performance.md`](preview-performance.md).
