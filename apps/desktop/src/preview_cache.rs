use std::mem::size_of;
use std::sync::Arc;

use rohditor_core::{
    CpuPreviewWorkspace, CropPolicy, DemosaicAlgorithm, DemosaicedBase, DisplayRgbImage,
    EditRecipe, MemoryEstimate, OutputPolicy, PreviewOptions, ReconstructedPreview, WhiteBalance,
};
use rohditor_raw::{RawFrame, RawOrientation, SourceIdentity};

/// Explicit keys for the four preview cache levels defined in the roadmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewCacheKeys {
    decoded: DecodedRawKey,
    reconstructed: ReconstructedCameraRgbKey,
    demosaiced: DemosaicedBaseKey,
    adjusted: AdjustedPreviewKey,
}

impl PreviewCacheKeys {
    pub(crate) fn new(
        document_id: u64,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Self {
        let decoded = DecodedRawKey {
            document_id,
            source_identity: frame.info.source_identity,
            width: frame.info.width,
            height: frame.info.height,
            row_stride: frame.row_stride,
            samples: frame.mosaic.len(),
        };
        let reconstructed = ReconstructedCameraRgbKey {
            decoded: decoded.clone(),
            crop_policy: options.render.crop_policy,
            max_long_edge: options.max_long_edge,
            algorithm: options.render.demosaic,
            // Bump when the retained source representation changes. The GPU
            // boundary now consumes camera-native samples rather than a
            // camera-converted base.
            reconstruction_version: 3,
        };
        let demosaiced = DemosaicedBaseKey {
            reconstructed: reconstructed.clone(),
            recipe_schema_version: recipe.schema_version,
            white_balance: WhiteBalanceKey::from(recipe.color.white_balance),
        };
        let adjusted = AdjustedPreviewKey {
            demosaiced: demosaiced.clone(),
            exposure_bits: recipe.light.exposure_ev.to_bits(),
            contrast_bits: recipe.light.contrast.to_bits(),
            highlights_bits: recipe.light.highlights.to_bits(),
            shadows_bits: recipe.light.shadows.to_bits(),
            whites_bits: recipe.light.whites.to_bits(),
            blacks_bits: recipe.light.blacks.to_bits(),
            tone_shadows_bits: recipe.light.tone_curve.shadows.to_bits(),
            tone_darks_bits: recipe.light.tone_curve.darks.to_bits(),
            tone_lights_bits: recipe.light.tone_curve.lights.to_bits(),
            tone_highlights_bits: recipe.light.tone_curve.highlights.to_bits(),
            saturation_bits: recipe.color.saturation.to_bits(),
            vibrance_bits: recipe.color.vibrance.to_bits(),
            hsl_bits: recipe
                .color
                .hsl
                .channels
                .iter()
                .flat_map(|channel| [channel.hue, channel.saturation, channel.luminance])
                .map(f32::to_bits)
                .collect(),
            grading_bits: recipe
                .color
                .grading
                .shadows
                .into_iter()
                .chain(recipe.color.grading.midtones)
                .chain(recipe.color.grading.highlights)
                .map(f32::to_bits)
                .collect(),
            orientation: recipe
                .geometry
                .orientation_override
                .unwrap_or(frame.info.orientation),
            output_policy: options.render.output_policy,
        };
        Self {
            decoded,
            reconstructed,
            demosaiced,
            adjusted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedRawKey {
    document_id: u64,
    source_identity: Option<SourceIdentity>,
    width: usize,
    height: usize,
    row_stride: usize,
    samples: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReconstructedCameraRgbKey {
    decoded: DecodedRawKey,
    crop_policy: CropPolicy,
    max_long_edge: usize,
    algorithm: DemosaicAlgorithm,
    reconstruction_version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DemosaicedBaseKey {
    reconstructed: ReconstructedCameraRgbKey,
    recipe_schema_version: u32,
    white_balance: WhiteBalanceKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhiteBalanceKey {
    AsShot,
    Manual {
        red_bits: u32,
        green_bits: u32,
        blue_bits: u32,
    },
    TemperatureTint {
        temperature_bits: u32,
        tint_bits: u32,
    },
}

impl From<WhiteBalance> for WhiteBalanceKey {
    fn from(value: WhiteBalance) -> Self {
        match value {
            WhiteBalance::AsShot => Self::AsShot,
            WhiteBalance::ManualMultipliers { red, green, blue } => Self::Manual {
                red_bits: red.to_bits(),
                green_bits: green.to_bits(),
                blue_bits: blue.to_bits(),
            },
            WhiteBalance::TemperatureTint { temperature, tint } => Self::TemperatureTint {
                temperature_bits: temperature.to_bits(),
                tint_bits: tint.to_bits(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdjustedPreviewKey {
    demosaiced: DemosaicedBaseKey,
    exposure_bits: u32,
    contrast_bits: u32,
    highlights_bits: u32,
    shadows_bits: u32,
    whites_bits: u32,
    blacks_bits: u32,
    tone_shadows_bits: u32,
    tone_darks_bits: u32,
    tone_lights_bits: u32,
    tone_highlights_bits: u32,
    saturation_bits: u32,
    vibrance_bits: u32,
    hsl_bits: Vec<u32>,
    grading_bits: Vec<u32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewCacheHits {
    pub decoded: bool,
    pub reconstructed: bool,
    pub demosaiced: bool,
    pub adjusted: bool,
}

#[derive(Debug)]
struct DecodedEntry {
    key: DecodedRawKey,
    frame: Arc<RawFrame>,
}

#[derive(Debug)]
struct ReconstructedEntry {
    key: ReconstructedCameraRgbKey,
    preview: ReconstructedPreview,
}

#[derive(Debug)]
struct DemosaicedEntry {
    key: DemosaicedBaseKey,
    base: DemosaicedBase,
}

#[derive(Debug)]
pub(crate) struct AdjustedPreviewEntry {
    key: AdjustedPreviewKey,
    pub image: DisplayRgbImage<u8>,
    pub memory: MemoryEstimate,
}

/// Bounded one-document preview cache with deterministic cascading eviction.
/// Each conceptual level retains at most one value.
#[derive(Debug, Default)]
pub(crate) struct PreviewCache {
    decoded: Option<DecodedEntry>,
    reconstructed: Option<ReconstructedEntry>,
    demosaiced: Option<DemosaicedEntry>,
    adjusted: Option<AdjustedPreviewEntry>,
    workspace: CpuPreviewWorkspace,
}

impl PreviewCache {
    /// Install the current decoded source key and evict every downstream level
    /// whose explicit key no longer matches.
    pub(crate) fn prepare(
        &mut self,
        keys: &PreviewCacheKeys,
        frame: &Arc<RawFrame>,
    ) -> PreviewCacheHits {
        let decoded = self
            .decoded
            .as_ref()
            .is_some_and(|entry| entry.key == keys.decoded);
        if !decoded {
            self.decoded = Some(DecodedEntry {
                key: keys.decoded.clone(),
                frame: Arc::clone(frame),
            });
            self.reconstructed = None;
            self.demosaiced = None;
            self.adjusted = None;
        }

        let reconstructed = self
            .reconstructed
            .as_ref()
            .is_some_and(|entry| entry.key == keys.reconstructed);
        if !reconstructed {
            self.reconstructed = None;
            self.demosaiced = None;
            self.adjusted = None;
        }

        let demosaiced = self
            .demosaiced
            .as_ref()
            .is_some_and(|entry| entry.key == keys.demosaiced);
        if !demosaiced {
            self.demosaiced = None;
            self.adjusted = None;
        }

        let adjusted = self
            .adjusted
            .as_ref()
            .is_some_and(|entry| entry.key == keys.adjusted);
        if !adjusted {
            self.adjusted = None;
        }

        PreviewCacheHits {
            decoded,
            reconstructed,
            demosaiced,
            adjusted,
        }
    }

    pub(crate) fn reconstructed(&self, keys: &PreviewCacheKeys) -> Option<&ReconstructedPreview> {
        self.reconstructed
            .as_ref()
            .filter(|entry| entry.key == keys.reconstructed)
            .map(|entry| &entry.preview)
    }

    pub(crate) fn insert_reconstructed(
        &mut self,
        keys: &PreviewCacheKeys,
        preview: ReconstructedPreview,
    ) {
        self.reconstructed = Some(ReconstructedEntry {
            key: keys.reconstructed.clone(),
            preview,
        });
        self.demosaiced = None;
        self.adjusted = None;
    }

    pub(crate) fn demosaiced(&self, keys: &PreviewCacheKeys) -> Option<&DemosaicedBase> {
        self.demosaiced
            .as_ref()
            .filter(|entry| entry.key == keys.demosaiced)
            .map(|entry| &entry.base)
    }

    pub(crate) fn insert_demosaiced(&mut self, keys: &PreviewCacheKeys, base: DemosaicedBase) {
        self.demosaiced = Some(DemosaicedEntry {
            key: keys.demosaiced.clone(),
            base,
        });
        self.adjusted = None;
    }

    pub(crate) fn adjusted(&self, keys: &PreviewCacheKeys) -> Option<&AdjustedPreviewEntry> {
        self.adjusted
            .as_ref()
            .filter(|entry| entry.key == keys.adjusted)
    }

    pub(crate) fn insert_adjusted(
        &mut self,
        keys: &PreviewCacheKeys,
        image: DisplayRgbImage<u8>,
        memory: MemoryEstimate,
    ) {
        self.adjusted = Some(AdjustedPreviewEntry {
            key: keys.adjusted.clone(),
            image,
            memory,
        });
    }

    pub(crate) fn base_and_workspace(
        &mut self,
        keys: &PreviewCacheKeys,
    ) -> Option<(&DemosaicedBase, &mut CpuPreviewWorkspace)> {
        let Self {
            demosaiced,
            workspace,
            ..
        } = self;
        demosaiced
            .as_ref()
            .filter(|entry| entry.key == keys.demosaiced)
            .map(|entry| (&entry.base, workspace))
    }

    pub(crate) fn workspace_reusable(&self, keys: &PreviewCacheKeys) -> bool {
        self.demosaiced(keys)
            .is_some_and(|base| self.workspace.can_reuse(base))
    }

    pub(crate) fn clear_document(&mut self, document_id: u64) {
        if self
            .decoded
            .as_ref()
            .is_some_and(|entry| entry.key.document_id == document_id)
        {
            *self = Self::default();
        }
    }

    /// Deterministic total of retained CPU image buffers. This is deliberately
    /// distinct from process RSS and may count a decoded `Arc` also held by UI.
    pub(crate) fn resident_bytes(&self) -> usize {
        let decoded = self.decoded.as_ref().map_or(0, |entry| {
            entry.frame.mosaic.len().saturating_mul(size_of::<u16>())
        });
        let reconstructed = self
            .reconstructed
            .as_ref()
            .map_or(0, |entry| entry.preview.buffer_bytes());
        let demosaiced = self
            .demosaiced
            .as_ref()
            .map_or(0, |entry| entry.base.buffer_bytes());
        let adjusted = self
            .adjusted
            .as_ref()
            .map_or(0, |entry| entry.image.data().len());
        decoded
            .saturating_add(reconstructed)
            .saturating_add(demosaiced)
            .saturating_add(adjusted)
            .saturating_add(self.workspace.buffer_bytes())
    }
}
