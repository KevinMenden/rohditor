# Remaining Rohditor bug findings

**Review date:** 2026-09-01
**Scope:** findings from the desktop preview, Light controls, tone curve, and Color mixer investigation.

The Source 1:1 toolbar-selection issue is fixed separately. The items below remain open and should not be lost while implementing that fix.

## 1. Source 1:1 quality and latency

### Observed behavior

- Source 1:1 takes noticeably longer than the fit preview.
- The zoomed source image can show more false colour or edge artifacts than the fit preview.
- Zooming back out from source resolution can look worse than the antialiased fit preview.

### Current cause

Source inspection is intentionally a separate, full-resolution CPU render. The desktop worker calls `render_source_scale_preview_cancellable`, clears the normal preview cache, and produces a complete 6000x4000 display image before installing it. A private A6400 run measured about 1.04 s and 413 MiB peak.

The fit preview is reconstructed and reduced with camera-linear area filtering. Source mode is displayed with a single full-resolution texture and ordinary linear sampling, without a mip/pyramid level. This aliases during minification and exposes demosaic detail that area reduction hides. MHC can also produce false colour around saturated, high-contrast edges because of its negative filter lobes.

Relevant paths:

- `apps/desktop/src/coordinator.rs`: `process_source_scale_preview`
- `crates/core/src/pipeline.rs`: `render_source_scale_preview_cancellable`
- `apps/desktop/src/ui/viewport.rs`: texture sampling and scaling

### Proposed solution

Short term:

- Keep the fit preview available while source inspection is prepared.
- Use the fit/area-reduced representation below 100% magnification and source pixels at or above 100%.
- Keep source-resolution mode and magnification independent in the status UI.

Long term:

- Build a tiled, mipmapped source pyramid.
- Develop only visible source tiles, with a demosaic border, instead of converting and uploading the entire frame for every inspection request.
- Apply downstream adjustments on the GPU where possible, retaining the CPU path as the correctness fallback.
- Add quality fixtures for diagonal edges, saturated boundaries, and image borders before adding any false-colour suppression.
