use lensfun::{
    CalibDistortion, CalibTca, CalibVignetting, DistortionModel as LensfunDistortion,
    TcaModel as LensfunTca, VignettingModel as LensfunVignetting,
};

use crate::catalog::{OpticsService, hash_f32, hash_text};
use crate::matching::resolve_record;
use crate::{
    CorrectionComponents, LensCorrectionPlan, OPTICS_ALGORITHM_VERSION, OpticsError, OpticsQuery,
    ProfileRequest,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DistortionModel {
    Poly3 { k1: f32 },
    Poly5 { k1: f32, k2: f32 },
    Ptlens { a: f32, b: f32, c: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TcaModel {
    Linear { kr: f32, kb: f32 },
    Poly3 { red: [f32; 3], blue: [f32; 3] },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum VignettingModel {
    Pa { k1: f32, k2: f32, k3: f32 },
}

#[derive(Debug, Clone, Copy)]
struct MappingContext {
    norm_scale: f64,
    norm_unscale: f64,
    center_x: f64,
    center_y: f64,
    scale: f32,
    distortion: Option<DistortionModel>,
    tca: Option<TcaModel>,
}

impl MappingContext {
    fn from_plan(plan: &LensCorrectionPlan) -> Self {
        Self {
            norm_scale: plan.norm_scale,
            norm_unscale: plan.norm_unscale,
            center_x: plan.center_x,
            center_y: plan.center_y,
            scale: plan.scale,
            distortion: plan.distortion,
            tca: plan.tca,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FingerprintInput<'a> {
    profile_id: &'a str,
    database_fingerprint: u64,
    requested: CorrectionComponents,
    applied: CorrectionComponents,
    width: usize,
    height: usize,
    scale: f32,
    norm_scale: f64,
    center_x: f64,
    center_y: f64,
    distortion: Option<DistortionModel>,
    tca: Option<TcaModel>,
    vignetting: Option<VignettingModel>,
    fallback: bool,
}

pub(crate) fn build_plan(
    service: &OpticsService,
    query: &OpticsQuery,
    request: ProfileRequest,
    requested: CorrectionComponents,
    width: usize,
    height: usize,
) -> Result<LensCorrectionPlan, OpticsError> {
    validate_dimensions(width, height)?;
    let explicit_id = match &request {
        ProfileRequest::Automatic => None,
        ProfileRequest::Explicit { profile_id } => Some(profile_id.as_str()),
    };
    let record = resolve_record(service, query, explicit_id)?;
    let camera = service.camera(record.camera_index);
    let lens = service.lens(record.lens_index);
    let focal = query
        .focal_length_mm
        .expect("resolve_record validates focal");
    let real_focal = lens
        .interpolate_distortion(focal)
        .and_then(|calibration| calibration.real_focal)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(focal);
    let crop = camera.crop_factor;
    if !crop.is_finite() || crop <= 0.0 {
        return Err(OpticsError::InvalidParameter {
            field: "camera.crop_factor",
            reason: "must be finite and positive".to_owned(),
        });
    }
    let focal = finite_positive("focal_length_mm", focal)?;
    let real_focal = finite_positive("real_focal", real_focal)?;
    let width_f = if width >= 2 { (width - 1) as f64 } else { 1.0 };
    let height_f = if height >= 2 {
        (height - 1) as f64
    } else {
        1.0
    };
    let norm_scale = 36.0_f64.hypot(24.0)
        / f64::from(crop)
        / (width_f + 1.0).hypot(height_f + 1.0)
        / f64::from(real_focal);
    if !norm_scale.is_finite() || norm_scale <= 0.0 {
        return Err(OpticsError::InvalidParameter {
            field: "normalization",
            reason: "computed a non-finite scale".to_owned(),
        });
    }
    let norm_unscale = 1.0 / norm_scale;
    let lens_size = width_f.min(height_f);
    let center_x = (width_f / 2.0 + lens_size / 2.0 * f64::from(lens.center_x)) * norm_scale;
    let center_y = (height_f / 2.0 + lens_size / 2.0 * f64::from(lens.center_y)) * norm_scale;

    let distortion = requested
        .distortion
        .then(|| lens.interpolate_distortion(focal))
        .flatten()
        .and_then(|calibration| {
            rescale_distortion(
                &calibration,
                lens.aspect_ratio,
                lens.crop_factor,
                real_focal,
            )
        });
    let tca = requested
        .chromatic_aberration
        .then(|| lens.interpolate_tca(focal))
        .flatten()
        .and_then(|calibration| {
            rescale_tca(
                &calibration,
                lens.aspect_ratio,
                lens.crop_factor,
                real_focal,
            )
        });
    let aperture = query
        .aperture_f_number
        .filter(|value| value.is_finite() && *value > 0.0);
    let focus = query
        .focus_distance_m
        .filter(|value| value.is_finite() && *value > 0.0);
    let focus_for_interpolation = focus.unwrap_or(1000.0);
    let vignetting = requested
        .vignetting
        .then(|| {
            aperture.and_then(|aperture| {
                lens.interpolate_vignetting(focal, aperture, focus_for_interpolation)
            })
        })
        .flatten()
        .and_then(|calibration| rescale_vignetting(&calibration, lens.crop_factor, real_focal));
    let used_infinity_distance_fallback = vignetting.is_some() && focus.is_none();

    let applied = CorrectionComponents {
        distortion: distortion.is_some(),
        vignetting: vignetting.is_some(),
        chromatic_aberration: tca.is_some(),
    };
    let scale = if distortion.is_some() || tca.is_some() {
        find_scale(
            width,
            height,
            MappingContext {
                norm_scale,
                norm_unscale,
                center_x,
                center_y,
                scale: 1.0,
                distortion,
                tca,
            },
        )?
    } else {
        1.0
    };
    let content_fingerprint = plan_fingerprint(FingerprintInput {
        profile_id: &record.summary.id,
        database_fingerprint: service.database_provenance().fingerprint,
        requested,
        applied,
        width,
        height,
        scale,
        norm_scale,
        center_x,
        center_y,
        distortion,
        tca,
        vignetting,
        fallback: used_infinity_distance_fallback,
    });

    Ok(LensCorrectionPlan {
        profile: record.summary.clone(),
        database: service.database_provenance().clone(),
        requested,
        applied,
        used_infinity_distance_fallback,
        content_fingerprint,
        width,
        height,
        norm_scale,
        norm_unscale,
        center_x,
        center_y,
        scale,
        distortion,
        tca,
        vignetting,
    })
}

fn validate_dimensions(width: usize, height: usize) -> Result<(), OpticsError> {
    if width < 4 || height < 4 {
        return Err(OpticsError::InvalidDimensions {
            width,
            height,
            reason: "cubic correction requires at least four pixels per axis".to_owned(),
        });
    }
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| OpticsError::InvalidDimensions {
            width,
            height,
            reason: "pixel count overflowed".to_owned(),
        })?;
    Ok(())
}

fn finite_positive(field: &'static str, value: f32) -> Result<f32, OpticsError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(OpticsError::InvalidParameter {
            field,
            reason: "must be finite and positive".to_owned(),
        })
    }
}

fn rescale_distortion(
    calibration: &CalibDistortion,
    aspect_ratio: f32,
    crop: f32,
    real_focal: f32,
) -> Option<DistortionModel> {
    let hugin_scale =
        36.0_f64.hypot(24.0) / f64::from(crop) / f64::from(aspect_ratio).hypot(1.0) / 2.0;
    let hs = f64::from(real_focal) / hugin_scale;
    match calibration.model {
        LensfunDistortion::None => None,
        LensfunDistortion::Poly3 { k1: 0.0 } => None,
        LensfunDistortion::Poly3 { k1 } => {
            let d = 1.0 - f64::from(k1);
            Some(DistortionModel::Poly3 {
                k1: (f64::from(k1) * hs.powi(2) / d.powi(3)) as f32,
            })
        }
        LensfunDistortion::Poly5 { k1, k2 } => Some(DistortionModel::Poly5 {
            k1: (f64::from(k1) * hs.powi(2)) as f32,
            k2: (f64::from(k2) * hs.powi(4)) as f32,
        }),
        LensfunDistortion::Ptlens { a, b, c } => {
            let d = 1.0 - f64::from(a) - f64::from(b) - f64::from(c);
            Some(DistortionModel::Ptlens {
                a: (f64::from(a) * hs.powi(3) / d.powi(4)) as f32,
                b: (f64::from(b) * hs.powi(2) / d.powi(3)) as f32,
                c: (f64::from(c) * hs / d.powi(2)) as f32,
            })
        }
    }
}

fn rescale_tca(
    calibration: &CalibTca,
    aspect_ratio: f32,
    crop: f32,
    real_focal: f32,
) -> Option<TcaModel> {
    let hugin_scale =
        36.0_f64.hypot(24.0) / f64::from(crop) / f64::from(aspect_ratio).hypot(1.0) / 2.0;
    let hs = f64::from(real_focal) / hugin_scale;
    match calibration.model {
        LensfunTca::None => None,
        LensfunTca::Linear { kr, kb }
            if kr.is_finite() && kb.is_finite() && kr != 0.0 && kb != 0.0 =>
        {
            Some(TcaModel::Linear {
                kr: 1.0 / kr,
                kb: 1.0 / kb,
            })
        }
        LensfunTca::Linear { .. } => None,
        LensfunTca::Poly3 { red, blue } => Some(TcaModel::Poly3 {
            red: [
                red[0],
                (f64::from(red[1]) * hs) as f32,
                (f64::from(red[2]) * hs.powi(2)) as f32,
            ],
            blue: [
                blue[0],
                (f64::from(blue[1]) * hs) as f32,
                (f64::from(blue[2]) * hs.powi(2)) as f32,
            ],
        }),
    }
}

fn rescale_vignetting(
    calibration: &CalibVignetting,
    crop: f32,
    real_focal: f32,
) -> Option<VignettingModel> {
    match calibration.model {
        LensfunVignetting::None => None,
        LensfunVignetting::Pa { k1, k2, k3 } => {
            let hugin_scale = 36.0_f64.hypot(24.0) / f64::from(crop) / 2.0;
            let hs = f64::from(real_focal) / hugin_scale;
            Some(VignettingModel::Pa {
                k1: (f64::from(k1) * hs.powi(2)) as f32,
                k2: (f64::from(k2) * hs.powi(4)) as f32,
                k3: (f64::from(k3) * hs.powi(6)) as f32,
            })
        }
    }
}

pub(crate) fn map_coordinates(
    plan: &LensCorrectionPlan,
    x: usize,
    y: usize,
) -> Result<[(f32, f32); 3], OpticsError> {
    map_coordinates_values(&MappingContext::from_plan(plan), x, y)
}

fn map_coordinates_values(
    mapping: &MappingContext,
    x: usize,
    y: usize,
) -> Result<[(f32, f32); 3], OpticsError> {
    let center_px_x = mapping.center_x * mapping.norm_unscale;
    let center_px_y = mapping.center_y * mapping.norm_unscale;
    let mapped_x = center_px_x + (x as f64 - center_px_x) / f64::from(mapping.scale);
    let mapped_y = center_px_y + (y as f64 - center_px_y) / f64::from(mapping.scale);
    let input_x = mapped_x * mapping.norm_scale - mapping.center_x;
    let input_y = mapped_y * mapping.norm_scale - mapping.center_y;
    let (geometry_x, geometry_y) =
        apply_distortion(mapping.distortion, input_x as f32, input_y as f32);
    if !geometry_x.is_finite() || !geometry_y.is_finite() {
        return Err(OpticsError::Correction {
            reason: "Lensfun distortion inversion did not converge".to_owned(),
        });
    }
    let normalized = apply_tca(mapping.tca, geometry_x, geometry_y);
    let mut result = [(0.0, 0.0); 3];
    for (index, (nx, ny)) in normalized.into_iter().enumerate() {
        let px = (f64::from(nx) + mapping.center_x) * mapping.norm_unscale;
        let py = (f64::from(ny) + mapping.center_y) * mapping.norm_unscale;
        result[index] = (px as f32, py as f32);
        if !result[index].0.is_finite() || !result[index].1.is_finite() {
            return Err(OpticsError::Correction {
                reason: "Lensfun generated a non-finite source coordinate".to_owned(),
            });
        }
    }
    Ok(result)
}

fn apply_distortion(model: Option<DistortionModel>, x: f32, y: f32) -> (f32, f32) {
    match model {
        None => (x, y),
        Some(DistortionModel::Poly3 { k1 }) => lensfun::mod_coord::undist_poly3(x, y, k1),
        Some(DistortionModel::Poly5 { k1, k2 }) => lensfun::mod_coord::undist_poly5(x, y, k1, k2),
        Some(DistortionModel::Ptlens { a, b, c }) => {
            lensfun::mod_coord::undist_ptlens(x, y, a, b, c)
        }
    }
}

fn apply_tca(model: Option<TcaModel>, x: f32, y: f32) -> [(f32, f32); 3] {
    match model {
        None => [(x, y); 3],
        Some(TcaModel::Linear { kr, kb }) => [(x * kr, y * kr), (x, y), (x * kb, y * kb)],
        Some(TcaModel::Poly3 { red, blue }) => {
            let (red_x, red_y, blue_x, blue_y) =
                lensfun::mod_subpix::tca_poly3_reverse(x, y, red, blue);
            [(red_x, red_y), (x, y), (blue_x, blue_y)]
        }
    }
}

fn find_scale(
    width: usize,
    height: usize,
    mut mapping: MappingContext,
) -> Result<f32, OpticsError> {
    let mut scale = 1.0_f32;
    for _ in 0..160 {
        mapping.scale = scale;
        if perimeter_is_safe(width, height, &mapping) {
            return Ok(scale);
        }
        scale *= 1.025;
    }
    Err(OpticsError::InvalidFootprint)
}

fn perimeter_is_safe(width: usize, height: usize, mapping: &MappingContext) -> bool {
    let mut points = Vec::with_capacity(260);
    let max_x = width - 1;
    let max_y = height - 1;
    for index in 0..64 {
        let t = index as f32 / 63.0;
        points.push((t * max_x as f32, 0.0));
        points.push((t * max_x as f32, max_y as f32));
        points.push((0.0, t * max_y as f32));
        points.push((max_x as f32, t * max_y as f32));
    }
    points.into_iter().all(|(x, y)| {
        let Ok(coords) = map_coordinates_values(mapping, x as usize, y as usize) else {
            return false;
        };
        coords.into_iter().all(|(source_x, source_y)| {
            source_x >= 1.0
                && source_y >= 1.0
                && source_x <= (width - 2) as f32
                && source_y <= (height - 2) as f32
        })
    })
}

fn plan_fingerprint(input: FingerprintInput<'_>) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    hash_text(&mut hash, input.profile_id);
    hash_text(&mut hash, &input.database_fingerprint.to_string());
    hash_text(&mut hash, &OPTICS_ALGORITHM_VERSION.to_string());
    hash_text(
        &mut hash,
        &format!(
            "{:?}{:?}{}x{}{}",
            input.requested, input.applied, input.width, input.height, input.fallback
        ),
    );
    hash_f32(&mut hash, input.scale);
    hash_text(&mut hash, &input.norm_scale.to_bits().to_string());
    hash_text(&mut hash, &input.center_x.to_bits().to_string());
    hash_text(&mut hash, &input.center_y.to_bits().to_string());
    hash_text(
        &mut hash,
        &format!(
            "{:?}{:?}{:?}",
            input.distortion, input.tca, input.vignetting
        ),
    );
    hash
}

#[cfg(test)]
mod tests {
    use lensfun::{Database, Modifier};

    use super::*;
    use crate::{CorrectionComponents, OpticsQuery, OpticsService, ProfileRequest};

    const WIDTH: usize = 16;
    const HEIGHT: usize = 12;

    fn sony_tamron_query() -> OpticsQuery {
        OpticsQuery {
            camera_make: "Sony".to_owned(),
            camera_model: "ILCE-6400".to_owned(),
            camera_clean_make: "Sony".to_owned(),
            camera_clean_model: "Alpha 6400".to_owned(),
            lens_make: Some("Tamron".to_owned()),
            lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
            focal_length_mm: Some(35.0),
            aperture_f_number: Some(2.8),
            focus_distance_m: Some(10.0),
        }
    }

    fn known_pair(database: &Database) -> (&lensfun::Camera, &lensfun::Lens) {
        let camera = database
            .cameras
            .iter()
            .find(|camera| camera.model == "ILCE-6400")
            .expect("bundled database should contain Sony ILCE-6400");
        let lens = database
            .lenses
            .iter()
            .find(|lens| {
                lens.model == "E 17-70mm F2.8 B070"
                    || lens
                        .model_localized
                        .values()
                        .any(|model| model == "Tamron 17-70mm F/2.8 Di III-A VC RXD")
            })
            .expect("bundled database should contain Tamron 17-70mm");
        (camera, lens)
    }

    #[test]
    fn distortion_coordinates_match_lensfun_at_unit_scale() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
        let plan = service
            .resolve_plan(
                &sony_tamron_query(),
                ProfileRequest::Automatic,
                CorrectionComponents {
                    distortion: true,
                    vignetting: false,
                    chromatic_aberration: false,
                },
                WIDTH,
                HEIGHT,
            )
            .expect("distortion plan");
        let database = Database::load_bundled().expect("bundled Lensfun data should load");
        let (camera, lens) = known_pair(&database);
        let mut modifier = Modifier::new(
            lens,
            35.0,
            camera.crop_factor,
            WIDTH as u32,
            HEIGHT as u32,
            true,
        );
        assert!(modifier.enable_distortion_correction(lens));
        let mut expected = vec![0.0_f32; WIDTH * HEIGHT * 2];
        assert!(modifier.apply_geometry_distortion(0.0, 0.0, WIDTH, HEIGHT, &mut expected,));
        let mapping = MappingContext {
            norm_scale: plan.norm_scale,
            norm_unscale: plan.norm_unscale,
            center_x: plan.center_x,
            center_y: plan.center_y,
            scale: 1.0,
            distortion: plan.distortion,
            tca: None,
        };

        for y in 2..HEIGHT - 2 {
            for x in 2..WIDTH - 2 {
                let ours =
                    map_coordinates_values(&mapping, x, y).expect("finite distortion coordinate");
                let offset = (y * WIDTH + x) * 2;
                assert!((ours[0].0 - expected[offset]).abs() < 2e-5);
                assert!((ours[0].1 - expected[offset + 1]).abs() < 2e-5);
            }
        }
    }

    #[test]
    fn tca_coordinates_match_lensfun_at_unit_scale() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
        let plan = service
            .resolve_plan(
                &sony_tamron_query(),
                ProfileRequest::Automatic,
                CorrectionComponents {
                    distortion: false,
                    vignetting: false,
                    chromatic_aberration: true,
                },
                WIDTH,
                HEIGHT,
            )
            .expect("TCA plan");
        let database = Database::load_bundled().expect("bundled Lensfun data should load");
        let (camera, lens) = known_pair(&database);
        let mut modifier = Modifier::new(
            lens,
            35.0,
            camera.crop_factor,
            WIDTH as u32,
            HEIGHT as u32,
            true,
        );
        assert!(modifier.enable_tca_correction(lens));
        let mut expected = vec![0.0_f32; WIDTH * HEIGHT * 6];
        assert!(modifier.apply_subpixel_distortion(0.0, 0.0, WIDTH, HEIGHT, &mut expected,));
        let mapping = MappingContext {
            norm_scale: plan.norm_scale,
            norm_unscale: plan.norm_unscale,
            center_x: plan.center_x,
            center_y: plan.center_y,
            scale: 1.0,
            distortion: None,
            tca: plan.tca,
        };

        for y in 2..HEIGHT - 2 {
            for x in 2..WIDTH - 2 {
                let ours = map_coordinates_values(&mapping, x, y).expect("finite TCA coordinate");
                let offset = (y * WIDTH + x) * 6;
                for channel in 0..3 {
                    assert!((ours[channel].0 - expected[offset + channel * 2]).abs() < 2e-5);
                    assert!((ours[channel].1 - expected[offset + channel * 2 + 1]).abs() < 2e-5);
                }
            }
        }
    }
}
