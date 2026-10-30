//! Fitting the Matched rendering (tone and colour) to a photo's embedded camera JPEG.

use image::ImageFormat;
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, RenderingAdjustments, RenderingProfileSelection,
};
use rohditor_image::{DisplayRgbImage, DisplayTransfer, ImageError, Orientation, OrientationMap};
use rohditor_look::{
    ChromaFit, ChromaFitError, Proxy, ToneFit, ToneFitError, check_chroma_improvement,
    common_aspect_crops, compare_proxies, fit_camera_chroma, fit_camera_tone, luminance_histogram,
};
use rohditor_raw::{EncodedPreview, RawFrame};
use thiserror::Error;

use crate::{
    CpuPipeline, DemosaicedBase, OutputPolicy, PipelineError, PreviewOptions, RenderOptions,
};

/// Long edge of the neutral render the fit compares against the JPEG. Matching
/// distributions needs far fewer pixels than a viewing preview.
const FIT_RENDER_LONG_EDGE: usize = 768;

/// Previews smaller than this say too little about the tone to fit. Same
/// floor as RawTherapee's auto-matched curve.
const MIN_PREVIEW_PIXELS: u64 = 19_200;
/// A preview much smaller than the sensor is a thumbnail whose tone may differ
/// from the full rendering (same ratio as RawTherapee).
const MAX_PREVIEW_REDUCTION: u64 = 33;

#[derive(Debug, Error)]
pub enum CameraLookError {
    #[error("the embedded preview is too small ({width}x{height}) to match against")]
    PreviewTooSmall { width: u32, height: u32 },
    #[error("could not decode the embedded preview: {0}")]
    Decode(#[from] image::ImageError),
    #[error(
        "the preview and the RAW image have different orientations, so they cannot be compared"
    )]
    FramingMismatch,
    #[error("could not render the neutral reference: {0}")]
    Render(#[from] PipelineError),
    #[error("camera tone fit rejected: {0}")]
    Rejected(#[from] ToneFitError),
}

/// Decode a camera preview into an upright sRGB8 image.
///
/// Embedded previews are stored in sensor orientation and rely on the EXIF
/// tag to be displayed upright, so `orientation` (the RAW's) is applied here to
/// line the preview up with Rohditor's own renders.
///
/// The preview is assumed to be sRGB-encoded. Cameras that write Adobe RGB
/// previews will fit slightly off in saturated colours, but luminance, which
/// is all the tone fit uses, is hardly affected.
pub fn decode_camera_preview(
    preview: &EncodedPreview,
    orientation: Orientation,
) -> Result<DisplayRgbImage<u8>, CameraLookError> {
    let decoded = image::load_from_memory_with_format(&preview.bytes, ImageFormat::Jpeg)?;
    let rgb = decoded.into_rgb8();
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    orient_rgb8(rgb.as_raw(), width, height, orientation).map_err(|error| {
        CameraLookError::Render(PipelineError::InvalidDimensions {
            width,
            height,
            row_stride: width * 3,
            reason: error.to_string(),
        })
    })
}

/// Physically apply an EXIF orientation to tightly packed RGB8 samples.
fn orient_rgb8(
    samples: &[u8],
    width: usize,
    height: usize,
    orientation: Orientation,
) -> Result<DisplayRgbImage<u8>, ImageError> {
    let map = OrientationMap::new(width, height, orientation)?;
    let (output_width, output_height) = map.output_dimensions();
    let mut data = Vec::with_capacity(output_width * output_height * 3);
    for y in 0..output_height {
        for x in 0..output_width {
            let (source_x, source_y) = map.source_coordinate_in_bounds(x, y);
            let start = (source_y * width + source_x) * 3;
            data.extend_from_slice(&samples[start..start + 3]);
        }
    }
    DisplayRgbImage::new(
        output_width,
        output_height,
        output_width * 3,
        DisplayTransfer::Srgb,
        data,
    )
}

/// Recipe used for the reference render: all controls neutral and the
/// identity base rendering, so the only tone left is the camera's. The camera
/// profile is the photo's own, since colour fitting must see its look.
fn neutral_reference_recipe(camera_profile: &CameraProfileSelection) -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe.color.camera_profile = camera_profile.clone();
    recipe
}

/// A photo's fitted camera look.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraLook {
    pub tone: ToneFit,
    /// Colour is fitted after the tone and may be refused (for example for a
    /// black-and-white picture style) while the tone is still good.
    pub chroma: Result<ChromaFit, ChromaFitError>,
}

impl CameraLook {
    /// Store the fit in a recipe's rendering group. A refused chroma fit
    /// clears any previous one, leaving the tone-only look.
    pub fn store_in(&self, rendering: &mut RenderingAdjustments) {
        rendering.camera_tone = Some(self.tone.tone.clone());
        rendering.camera_chroma = self.chroma.as_ref().ok().map(|fit| fit.chroma);
    }
}

/// Fit the camera look for one photo.
///
/// Renders the RAW with the neutral base, decodes the embedded JPEG, crops
/// both to a common framing, and matches their luminance distributions. A
/// second render with the fitted tone is then compared with the JPEG to fit the
/// colourfulness per brightness. The result is meant to be stored in the recipe
/// (see [`CameraLook::store_in`]) so the fit runs once per photo.
pub fn match_camera_look(
    pipeline: &CpuPipeline,
    frame: &RawFrame,
    preview: &EncodedPreview,
    camera_profile: &CameraProfileSelection,
) -> Result<CameraLook, CameraLookError> {
    let preview_pixels = u64::from(preview.width) * u64::from(preview.height);
    if preview_pixels < MIN_PREVIEW_PIXELS
        || u64::from(preview.width) * MAX_PREVIEW_REDUCTION < frame.info.width as u64
    {
        return Err(CameraLookError::PreviewTooSmall {
            width: preview.width,
            height: preview.height,
        });
    }
    let camera = decode_camera_preview(preview, frame.info.orientation)?;

    let options = PreviewOptions {
        render: RenderOptions::default(),
        max_long_edge: FIT_RENDER_LONG_EDGE,
    };
    let mut recipe = neutral_reference_recipe(camera_profile);
    let base = pipeline.prepare_preview_base(frame, &recipe, options)?;
    let neutral = pipeline.render_preview_from_base(&base, &recipe, OutputPolicy::ClipToSrgb)?;

    let (neutral_crop, camera_crop) = common_aspect_crops(
        (neutral.image.width(), neutral.image.height()),
        (camera.width(), camera.height()),
    )
    .ok_or(CameraLookError::FramingMismatch)?;
    let tone = fit_camera_tone(
        &luminance_histogram(&neutral.image, neutral_crop),
        &luminance_histogram(&camera, camera_crop),
    )?;

    // Colour is judged on a render that already has the camera's tone, so
    // equal brightness bins hold comparable pixels.
    recipe.rendering.profile = RenderingProfileSelection::MATCHED;
    recipe.rendering.camera_tone = Some(tone.tone.clone());
    let toned = pipeline.render_preview_from_base(&base, &recipe, OutputPolicy::ClipToSrgb)?;
    let chroma = fit_camera_chroma(&toned.image, neutral_crop, &camera, camera_crop);
    let chroma = match chroma {
        Ok(fit) => gate_chroma(pipeline, &base, &recipe, &toned.image, &camera, fit),
        Err(error) => Err(error),
    };
    Ok(CameraLook { tone, chroma })
}

/// Keep a fitted chroma curve only if rendering with it is measurably closer
/// to the camera preview than the tone-only rendering, judged on binned pixels.
fn gate_chroma(
    pipeline: &CpuPipeline,
    base: &DemosaicedBase,
    toned_recipe: &EditRecipe,
    toned: &DisplayRgbImage<u8>,
    camera: &DisplayRgbImage<u8>,
    fit: ChromaFit,
) -> Result<ChromaFit, ChromaFitError> {
    let mean_error = |image: &DisplayRgbImage<u8>| {
        let (proxy, camera_proxy) = Proxy::pair(image, camera).ok()?;
        compare_proxies(&proxy, &camera_proxy).map(|difference| difference.mean_color_error)
    };
    let mut recipe = toned_recipe.clone();
    recipe.rendering.camera_chroma = Some(fit.chroma);
    let colored = pipeline
        .render_preview_from_base(base, &recipe, OutputPolicy::ClipToSrgb)
        .ok();
    let (Some(before), Some(after)) = (
        mean_error(toned),
        colored.and_then(|rendered| mean_error(&rendered.image)),
    ) else {
        return Err(ChromaFitError::NoMeasurableColor);
    };
    check_chroma_improvement(before, after).map(|()| fit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orienting_a_preview_moves_pixels_like_the_pipeline_does() {
        // 2x3 image whose pixels are numbered by position (value = index).
        let samples: Vec<u8> = (0..6_u8).flat_map(|index| [index; 3]).collect();
        let upright = orient_rgb8(&samples, 2, 3, Orientation::Normal).expect("normal");
        assert_eq!(upright.data(), samples);

        let rotated = orient_rgb8(&samples, 2, 3, Orientation::Rotate90).expect("rotate 90");
        assert_eq!((rotated.width(), rotated.height()), (3, 2));
        // Same convention as OrientationMap: output (0, 0) comes from source (0, 2).
        assert_eq!(rotated.pixel(0, 0), Some(&[4, 4, 4][..]));
        assert_eq!(rotated.pixel(2, 1), Some(&[1, 1, 1][..]));
    }
}
