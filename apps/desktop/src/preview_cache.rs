use std::mem::size_of;
use std::sync::Arc;

use rohditor_core::{
    CameraProfileKey, CpuPreviewWorkspace, DemosaicedBase, LOCAL_RATIOS_ALGORITHM_VERSION,
    MemoryEstimate, OPPOSED_ALGORITHM_VERSION, OutputPolicy, PreviewOptions, RawCropPolicy,
    ReconstructedPreview, camera_profile_key,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{EditRecipe, HighlightMethod, WhiteBalance};
use rohditor_image::{DisplayRgbImage, Orientation};
use rohditor_raw::{RawFrame, SourceIdentity};

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
            raw_crop_policy: options.render.raw_crop_policy,
            max_long_edge: options.max_long_edge,
            algorithm: options.render.demosaic,
            highlight: HighlightKey::from_recipe(recipe),
            // Bump when the retained source representation changes. The GPU
            // boundary now consumes camera-native samples rather than a
            // camera-converted base.
            reconstruction_version: 7,
        };
        let demosaiced = DemosaicedBaseKey {
            reconstructed: reconstructed.clone(),
            recipe_schema_version: recipe.schema_version,
            white_balance: WhiteBalanceKey::from(recipe.color.white_balance),
            camera_profile: camera_profile_key(&recipe.color.camera_profile),
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
            crop_bits: recipe.geometry.crop.map(|crop| {
                [
                    crop.left.to_bits(),
                    crop.top.to_bits(),
                    crop.right.to_bits(),
                    crop.bottom.to_bits(),
                ]
            }),
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
    raw_crop_policy: RawCropPolicy,
    max_long_edge: usize,
    algorithm: DemosaicAlgorithm,
    highlight: HighlightKey,
    reconstruction_version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HighlightKey {
    Off,
    Clip {
        threshold_bits: u32,
        white_balance: WhiteBalanceKey,
        camera_profile: Option<CameraProfileKey>,
    },
    LocalRatios {
        detection_threshold_bits: u32,
        algorithm_version: u8,
    },
    Opposed {
        detection_threshold_bits: u32,
        algorithm_version: u8,
    },
}

impl HighlightKey {
    fn from_recipe(recipe: &EditRecipe) -> Self {
        match recipe.raw.highlights.method {
            HighlightMethod::Off => Self::Off,
            HighlightMethod::Clip => Self::Clip {
                threshold_bits: recipe.raw.highlights.clip.threshold.to_bits(),
                white_balance: WhiteBalanceKey::from(recipe.color.white_balance),
                camera_profile: matches!(
                    recipe.color.white_balance,
                    WhiteBalance::TemperatureTint { .. }
                )
                .then(|| camera_profile_key(&recipe.color.camera_profile)),
            },
            HighlightMethod::LocalRatios => Self::LocalRatios {
                detection_threshold_bits: recipe
                    .raw
                    .highlights
                    .local_ratios
                    .detection_threshold
                    .to_bits(),
                algorithm_version: LOCAL_RATIOS_ALGORITHM_VERSION,
            },
            HighlightMethod::Opposed => Self::Opposed {
                detection_threshold_bits: recipe
                    .raw
                    .highlights
                    .opposed
                    .detection_threshold
                    .to_bits(),
                algorithm_version: OPPOSED_ALGORITHM_VERSION,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DemosaicedBaseKey {
    reconstructed: ReconstructedCameraRgbKey,
    recipe_schema_version: u32,
    white_balance: WhiteBalanceKey,
    camera_profile: CameraProfileKey,
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
    orientation: Orientation,
    crop_bits: Option<[u64; 4]>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_camera_profile::{CalibrationIlluminant, MatrixCalibration, MatrixCameraProfile};
    use rohditor_image::{BayerPattern, Orientation};
    use rohditor_raw::{
        CaptureMetadata, CfaPattern, LevelPattern, PhotometricInterpretation, RawFileInfo,
    };

    fn frame() -> RawFrame {
        RawFrame {
            info: RawFileInfo {
                format: "fixture".to_owned(),
                make: "Rohditor".to_owned(),
                model: "cache fixture".to_owned(),
                clean_make: "Rohditor".to_owned(),
                clean_model: "cache fixture".to_owned(),
                source_size_bytes: 0,
                source_identity: None,
                width: 2,
                height: 2,
                components_per_pixel: 1,
                source_bits_per_sample: Some(12),
                decoded_bits_per_sample: 16,
                compression: None,
                active_area: None,
                crop_area: None,
                photometric_interpretation: PhotometricInterpretation::Cfa {
                    pattern: CfaPattern {
                        name: BayerPattern::Rggb.name().to_owned(),
                        width: 2,
                        height: 2,
                    },
                },
                black_levels: LevelPattern {
                    values: vec![0.0; 4],
                    repeat_width: 2,
                    repeat_height: 2,
                    components_per_pixel: 1,
                },
                white_levels: vec![1.0; 4],
                as_shot_white_balance: [Some(2.0), Some(1.0), Some(1.5), None],
                xyz_to_camera: [[0.0; 3]; 4],
                color_matrices: Vec::new(),
                orientation: Orientation::Normal,
                capture: CaptureMetadata::default(),
                embedded_preview: None,
            },
            row_stride: 2,
            mosaic: Arc::from([0_u16, 0, 0, 0]),
        }
    }

    fn keys(recipe: &EditRecipe) -> PreviewCacheKeys {
        PreviewCacheKeys::new(7, &frame(), recipe, PreviewOptions::default())
    }

    #[test]
    fn highlight_cache_key_tracks_only_the_dependencies_of_reconstruction() {
        let off = EditRecipe::default();
        let mut off_wb = off.clone();
        off_wb.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 1.2,
            green: 1.0,
            blue: 0.8,
        };
        let off_keys = keys(&off);
        let off_wb_keys = keys(&off_wb);
        assert_eq!(off_keys.reconstructed, off_wb_keys.reconstructed);
        assert_ne!(off_keys.demosaiced, off_wb_keys.demosaiced);

        let mut off_threshold = off.clone();
        off_threshold.raw.highlights.clip.threshold = 1.25;
        assert_eq!(off_keys.reconstructed, keys(&off_threshold).reconstructed);

        let mut clip = off.clone();
        clip.raw.highlights.method = HighlightMethod::Clip;
        let clip_keys = keys(&clip);
        assert_ne!(off_keys.reconstructed, clip_keys.reconstructed);

        let same_clip_keys = keys(&clip);
        assert_eq!(clip_keys.reconstructed, same_clip_keys.reconstructed);

        let mut clip_wb = clip.clone();
        clip_wb.color.white_balance = off_wb.color.white_balance;
        assert_ne!(clip_keys.reconstructed, keys(&clip_wb).reconstructed);

        let mut clip_threshold = clip.clone();
        clip_threshold.raw.highlights.clip.threshold = 1.25;
        assert_ne!(clip_keys.reconstructed, keys(&clip_threshold).reconstructed);

        let mut local = off.clone();
        local.raw.highlights.method = HighlightMethod::LocalRatios;
        let local_keys = keys(&local);
        assert_ne!(off_keys.reconstructed, local_keys.reconstructed);
        assert_eq!(local_keys.reconstructed, keys(&local).reconstructed);
        assert_eq!(
            local_keys.reconstructed,
            keys(&{
                let mut changed_wb = local.clone();
                changed_wb.color.white_balance = off_wb.color.white_balance;
                changed_wb
            })
            .reconstructed
        );
        let mut local_threshold = local;
        local_threshold
            .raw
            .highlights
            .local_ratios
            .detection_threshold = 1.25;
        assert_ne!(
            local_keys.reconstructed,
            keys(&local_threshold).reconstructed
        );

        let mut opposed = off.clone();
        opposed.raw.highlights.method = HighlightMethod::Opposed;
        let opposed_keys = keys(&opposed);
        assert_ne!(local_keys.reconstructed, opposed_keys.reconstructed);
        assert_ne!(off_keys.reconstructed, opposed_keys.reconstructed);
        assert_eq!(opposed_keys.reconstructed, keys(&opposed).reconstructed);
        assert_eq!(
            opposed_keys.reconstructed,
            keys(&{
                let mut changed_wb = opposed.clone();
                changed_wb.color.white_balance = off_wb.color.white_balance;
                changed_wb
            })
            .reconstructed
        );
        let mut opposed_threshold = opposed;
        opposed_threshold.raw.highlights.opposed.detection_threshold = 1.25;
        assert_ne!(
            opposed_keys.reconstructed,
            keys(&opposed_threshold).reconstructed
        );
    }

    #[test]
    fn profile_cache_dependencies_follow_the_clip_temperature_exception() {
        let first = camera_profile(0.0, 'a');
        let second = camera_profile(0.1, 'b');

        let mut automatic = EditRecipe::default();
        let mut selected = automatic.clone();
        selected.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(first.clone());
        assert_eq!(
            keys(&automatic).reconstructed,
            keys(&selected).reconstructed,
        );
        assert_ne!(keys(&automatic).demosaiced, keys(&selected).demosaiced);

        let mut selected_other = selected.clone();
        selected_other.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(second.clone());
        assert_eq!(
            keys(&selected).reconstructed,
            keys(&selected_other).reconstructed,
        );
        assert_ne!(keys(&selected).demosaiced, keys(&selected_other).demosaiced);

        automatic.raw.highlights.method = HighlightMethod::Clip;
        automatic.color.white_balance = WhiteBalance::AsShot;
        selected = automatic.clone();
        selected.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(first.clone());
        selected_other = selected.clone();
        selected_other.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(second.clone());
        assert_eq!(
            keys(&selected).reconstructed,
            keys(&selected_other).reconstructed,
        );

        for method in [HighlightMethod::LocalRatios, HighlightMethod::Opposed] {
            let mut dynamic = EditRecipe::default();
            dynamic.raw.highlights.method = method;
            dynamic.color.white_balance = WhiteBalance::TemperatureTint {
                temperature: 5_500.0,
                tint: 0.1,
            };
            let mut dynamic_other = dynamic.clone();
            dynamic.color.camera_profile =
                rohditor_edit::CameraProfileSelection::Matrix(first.clone());
            dynamic_other.color.camera_profile =
                rohditor_edit::CameraProfileSelection::Matrix(second.clone());
            assert_eq!(
                keys(&dynamic).reconstructed,
                keys(&dynamic_other).reconstructed,
                "{method:?} should reuse camera-native RGB"
            );
        }

        let mut clipped_temperature = EditRecipe::default();
        clipped_temperature.raw.highlights.method = HighlightMethod::Clip;
        clipped_temperature.color.white_balance = WhiteBalance::TemperatureTint {
            temperature: 5_500.0,
            tint: 0.1,
        };
        clipped_temperature.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(first);
        let mut clipped_temperature_other = clipped_temperature.clone();
        clipped_temperature_other.color.camera_profile =
            rohditor_edit::CameraProfileSelection::Matrix(second);
        assert_ne!(
            keys(&clipped_temperature).reconstructed,
            keys(&clipped_temperature_other).reconstructed,
        );
    }

    fn camera_profile(matrix_offset: f32, digest: char) -> MatrixCameraProfile {
        MatrixCameraProfile {
            format_version: 1,
            source_sha256: digest.to_string().repeat(64),
            name: "Cache profile".to_owned(),
            camera_model: "Rohditor cache fixture".to_owned(),
            copyright: None,
            calibrations: vec![MatrixCalibration {
                illuminant: CalibrationIlluminant::D65,
                xyz_to_camera: [
                    [1.0 + matrix_offset, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                forward_camera_to_xyz_d50: None,
            }],
        }
    }
}
