use std::mem::size_of;
use std::sync::Arc;

use rohditor_core::{
    CpuPreviewWorkspace, CropPolicy, DemosaicAlgorithm, DemosaicedBase, DisplayRgbImage,
    EditRecipe, MemoryEstimate, NormalizedPreview, OutputPolicy, PreviewOptions, WhiteBalance,
};
use rohditor_raw::{RawFrame, RawOrientation, SourceIdentity};

/// Explicit keys for the four preview cache levels defined in the roadmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewCacheKeys {
    decoded: DecodedRawKey,
    normalized: NormalizedMosaicKey,
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
        let normalized = NormalizedMosaicKey {
            decoded: decoded.clone(),
            crop_policy: options.render.crop_policy,
            max_long_edge: options.max_long_edge,
            normalization_version: 1,
        };
        let demosaiced = DemosaicedBaseKey {
            normalized: normalized.clone(),
            recipe_schema_version: recipe.schema_version,
            white_balance: WhiteBalanceKey::from(recipe.white_balance),
            algorithm: options.render.demosaic,
        };
        let adjusted = AdjustedPreviewKey {
            demosaiced: demosaiced.clone(),
            exposure_bits: recipe.exposure_ev.to_bits(),
            contrast_bits: recipe.contrast.to_bits(),
            saturation_bits: recipe.saturation.to_bits(),
            orientation: recipe
                .orientation_override
                .unwrap_or(frame.info.orientation),
            output_policy: options.render.output_policy,
        };
        Self {
            decoded,
            normalized,
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
struct NormalizedMosaicKey {
    decoded: DecodedRawKey,
    crop_policy: CropPolicy,
    max_long_edge: usize,
    normalization_version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DemosaicedBaseKey {
    normalized: NormalizedMosaicKey,
    recipe_schema_version: u32,
    white_balance: WhiteBalanceKey,
    algorithm: DemosaicAlgorithm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhiteBalanceKey {
    AsShot,
    Manual {
        red_bits: u32,
        green_bits: u32,
        blue_bits: u32,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdjustedPreviewKey {
    demosaiced: DemosaicedBaseKey,
    exposure_bits: u32,
    contrast_bits: u32,
    saturation_bits: u32,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewCacheHits {
    pub decoded: bool,
    pub normalized: bool,
    pub demosaiced: bool,
    pub adjusted: bool,
}

#[derive(Debug)]
struct DecodedEntry {
    key: DecodedRawKey,
    frame: Arc<RawFrame>,
}

#[derive(Debug)]
struct NormalizedEntry {
    key: NormalizedMosaicKey,
    preview: NormalizedPreview,
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
    normalized: Option<NormalizedEntry>,
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
            self.normalized = None;
            self.demosaiced = None;
            self.adjusted = None;
        }

        let normalized = self
            .normalized
            .as_ref()
            .is_some_and(|entry| entry.key == keys.normalized);
        if !normalized {
            self.normalized = None;
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
            normalized,
            demosaiced,
            adjusted,
        }
    }

    pub(crate) fn normalized(&self, keys: &PreviewCacheKeys) -> Option<&NormalizedPreview> {
        self.normalized
            .as_ref()
            .filter(|entry| entry.key == keys.normalized)
            .map(|entry| &entry.preview)
    }

    pub(crate) fn insert_normalized(
        &mut self,
        keys: &PreviewCacheKeys,
        preview: NormalizedPreview,
    ) {
        self.normalized = Some(NormalizedEntry {
            key: keys.normalized.clone(),
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
        let normalized = self
            .normalized
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
            .saturating_add(normalized)
            .saturating_add(demosaiced)
            .saturating_add(adjusted)
            .saturating_add(self.workspace.buffer_bytes())
    }
}
