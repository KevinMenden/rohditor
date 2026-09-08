use rohditor_edit::{LensProfileSelection, OpticsAdjustments};
use rohditor_image::LinearRgbImage;
use rohditor_optics::{
    Cancellation, CorrectionComponents, OpticsError, OpticsProvenance, OpticsQuery, OpticsService,
    ProfileRequest,
};
use rohditor_raw::RawFileInfo;

use crate::{CancellationToken, PipelineError};

pub(crate) struct OpticsApplication {
    pub(crate) image: LinearRgbImage<f32>,
    pub(crate) provenance: Option<OpticsProvenance>,
    pub(crate) output_bytes: usize,
    pub(crate) scratch_bytes: usize,
}

impl Cancellation for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
}

/// Convert normalized RAW metadata into the optics domain query.
#[must_use]
pub fn optics_query_from_info(info: &RawFileInfo) -> OpticsQuery {
    OpticsQuery {
        camera_make: info.make.clone(),
        camera_model: info.model.clone(),
        camera_clean_make: info.clean_make.clone(),
        camera_clean_model: info.clean_model.clone(),
        lens_make: info.capture.lens_make.clone(),
        lens_model: info.capture.lens_model.clone(),
        focal_length_mm: info
            .capture
            .focal_length
            .and_then(|value| value.as_f64())
            .map(|value| value as f32),
        aperture_f_number: info
            .capture
            .aperture
            .and_then(|value| value.as_f64())
            .map(|value| value as f32),
        focus_distance_m: info
            .capture
            .focus_distance
            .and_then(|value| value.as_f64())
            .map(|value| value as f32),
    }
}

pub(crate) fn requested_components(adjustments: &OpticsAdjustments) -> CorrectionComponents {
    CorrectionComponents {
        distortion: adjustments.distortion,
        vignetting: adjustments.vignetting,
        chromatic_aberration: adjustments.chromatic_aberration,
    }
}

pub(crate) fn cache_provenance(
    service: Option<&OpticsService>,
    frame: &rohditor_raw::RawFrame,
    recipe: &rohditor_edit::EditRecipe,
    options: crate::RenderOptions,
) -> Option<OpticsProvenance> {
    if matches!(recipe.optics.profile, LensProfileSelection::Off) {
        return None;
    }
    let service = service?;
    let (width, height) =
        crate::cpu::raw_crop_dimensions(&frame.info, options.raw_crop_policy).ok()?;
    let request = match &recipe.optics.profile {
        LensProfileSelection::Automatic => ProfileRequest::Automatic,
        LensProfileSelection::Lensfun { profile_id } => ProfileRequest::Explicit {
            profile_id: profile_id.clone(),
        },
        LensProfileSelection::Off => return None,
    };
    service
        .resolve_plan(
            &optics_query_from_info(&frame.info),
            request,
            requested_components(&recipe.optics),
            width,
            height,
        )
        .ok()
        .map(|plan| plan.provenance())
}

pub(crate) fn apply_cancellable(
    service: Option<&OpticsService>,
    info: &RawFileInfo,
    adjustments: &OpticsAdjustments,
    image: LinearRgbImage<f32>,
    cancellation: &CancellationToken,
) -> Result<OpticsApplication, PipelineError> {
    if matches!(adjustments.profile, LensProfileSelection::Off) {
        return Ok(OpticsApplication {
            image,
            provenance: None,
            output_bytes: 0,
            scratch_bytes: 0,
        });
    }
    let service = service.ok_or_else(|| PipelineError::Optics {
        reason: "the Lensfun database is unavailable; optics must be disabled".to_owned(),
    })?;
    let request = match &adjustments.profile {
        LensProfileSelection::Automatic => ProfileRequest::Automatic,
        LensProfileSelection::Lensfun { profile_id } => ProfileRequest::Explicit {
            profile_id: profile_id.clone(),
        },
        LensProfileSelection::Off => unreachable!("handled above"),
    };
    let plan = service
        .resolve_plan(
            &optics_query_from_info(info),
            request,
            requested_components(adjustments),
            image.width(),
            image.height(),
        )
        .map_err(map_error)?;
    let result = plan
        .apply_cancellable(image, cancellation)
        .map_err(map_error)?;
    Ok(OpticsApplication {
        image: result.image,
        provenance: Some(result.provenance),
        output_bytes: result.output_bytes,
        scratch_bytes: result.scratch_bytes,
    })
}

fn map_error(error: OpticsError) -> PipelineError {
    match error {
        OpticsError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::Optics {
            reason: other.to_string(),
        },
    }
}

pub(crate) fn matches_recipe(
    provenance: Option<&OpticsProvenance>,
    adjustments: &OpticsAdjustments,
) -> bool {
    match (&adjustments.profile, provenance) {
        (LensProfileSelection::Off, None) => true,
        (LensProfileSelection::Off, Some(_)) => false,
        (_, None) => false,
        (LensProfileSelection::Automatic, Some(provenance)) => {
            provenance.requested == requested_components(adjustments)
        }
        (LensProfileSelection::Lensfun { profile_id }, Some(provenance)) => {
            provenance.profile.id == *profile_id
                && provenance.requested == requested_components(adjustments)
        }
    }
}

pub(crate) fn optics_enabled(adjustments: &OpticsAdjustments) -> bool {
    !matches!(adjustments.profile, LensProfileSelection::Off)
}
