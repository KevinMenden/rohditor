use crate::catalog::{OpticsService, ProfileRecord, normalized};
use crate::{LensProfileSummary, MetadataField, OpticsError, OpticsQuery, ProfileMatch};

const MANUAL_CANDIDATE_LIMIT: usize = 128;

pub(crate) fn profile_match(service: &OpticsService, query: &OpticsQuery) -> ProfileMatch {
    match candidate_records(service, query) {
        CandidateRecords::Missing(fields) => ProfileMatch::MissingMetadata { fields },
        CandidateRecords::CameraNotFound => ProfileMatch::CameraNotFound,
        CandidateRecords::LensNotFound => ProfileMatch::LensNotFound,
        CandidateRecords::Candidates(candidates) => {
            let summaries = candidates
                .into_iter()
                .map(|record| record.summary.clone())
                .collect::<Vec<_>>();
            match summaries.as_slice() {
                [summary] => ProfileMatch::Unique(summary.clone()),
                _ => ProfileMatch::Ambiguous(summaries),
            }
        }
    }
}

pub(crate) fn manual_candidates(
    service: &OpticsService,
    query: &OpticsQuery,
) -> Vec<LensProfileSummary> {
    let cameras = matching_camera_indices(service, query);
    if cameras.is_empty() {
        return Vec::new();
    }
    let focal = query
        .focal_length_mm
        .filter(|value| value.is_finite() && *value > 0.0);
    service
        .records
        .iter()
        .filter(|record| cameras.contains(&record.camera_index))
        .filter(|record| {
            focal.is_none_or(|value| focal_in_range(service.lens(record.lens_index), value))
        })
        .take(MANUAL_CANDIDATE_LIMIT)
        .map(|record| record.summary.clone())
        .collect()
}

pub(crate) fn resolve_record<'a>(
    service: &'a OpticsService,
    query: &OpticsQuery,
    explicit_id: Option<&str>,
) -> Result<&'a ProfileRecord, OpticsError> {
    if query.camera_model.trim().is_empty() && query.camera_clean_model.trim().is_empty() {
        return Err(OpticsError::MissingMetadata {
            field: MetadataField::CameraModel,
        });
    }
    if explicit_id.is_none()
        && query
            .lens_model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(OpticsError::MissingMetadata {
            field: MetadataField::LensModel,
        });
    }
    let focal = query.focal_length_mm.ok_or(OpticsError::MissingMetadata {
        field: MetadataField::FocalLength,
    })?;
    if !focal.is_finite() || focal <= 0.0 {
        return Err(OpticsError::MissingMetadata {
            field: MetadataField::FocalLength,
        });
    }

    let cameras = matching_camera_indices(service, query);
    if cameras.is_empty() {
        return Err(OpticsError::CameraNotFound);
    }

    if let Some(explicit_id) = explicit_id {
        // An explicit profile is the user's override for missing or incorrect
        // lens EXIF. Camera compatibility and focal range remain safety
        // checks, but the selected ID intentionally replaces lens-name
        // matching rather than being rejected by it.
        let Some(record) = service
            .records
            .iter()
            .find(|record| record.summary.id == explicit_id)
        else {
            return Err(OpticsError::ProfileNotFound {
                profile_id: explicit_id.to_owned(),
            });
        };
        if !cameras.contains(&record.camera_index) {
            return Err(OpticsError::IncompatibleProfile {
                reason: "the profile belongs to a different camera".to_owned(),
            });
        }
        if !focal_in_range(service.lens(record.lens_index), focal) {
            return Err(OpticsError::IncompatibleProfile {
                reason: "the captured focal length is outside the profile range".to_owned(),
            });
        }
        return Ok(record);
    }

    let records = service
        .records
        .iter()
        .filter(|record| cameras.contains(&record.camera_index))
        .filter(|record| record_matches_lens(service, record, query))
        .filter(|record| focal_in_range(service.lens(record.lens_index), focal))
        .collect::<Vec<_>>();

    match records.as_slice() {
        [record] => Ok(record),
        [] => Err(OpticsError::LensNotFound),
        candidates => Err(OpticsError::Ambiguous {
            count: candidates.len(),
        }),
    }
}

enum CandidateRecords<'a> {
    Missing(Vec<MetadataField>),
    CameraNotFound,
    LensNotFound,
    Candidates(Vec<&'a ProfileRecord>),
}

fn candidate_records<'a>(service: &'a OpticsService, query: &OpticsQuery) -> CandidateRecords<'a> {
    let mut missing = Vec::new();
    if query.camera_model.trim().is_empty() && query.camera_clean_model.trim().is_empty() {
        missing.push(MetadataField::CameraModel);
    }
    if query
        .lens_model
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        missing.push(MetadataField::LensModel);
    }
    if query
        .focal_length_mm
        .is_none_or(|value| !value.is_finite() || value <= 0.0)
    {
        missing.push(MetadataField::FocalLength);
    }
    if !missing.is_empty() {
        return CandidateRecords::Missing(missing);
    }

    let cameras = matching_camera_indices(service, query);
    if cameras.is_empty() {
        return CandidateRecords::CameraNotFound;
    }
    let focal = query.focal_length_mm.expect("validated above");
    let records = service
        .records
        .iter()
        .filter(|record| cameras.contains(&record.camera_index))
        .filter(|record| record_matches_lens(service, record, query))
        .filter(|record| focal_in_range(service.lens(record.lens_index), focal))
        .collect::<Vec<_>>();
    if records.is_empty() {
        CandidateRecords::LensNotFound
    } else {
        CandidateRecords::Candidates(records)
    }
}

fn matching_camera_indices(service: &OpticsService, query: &OpticsQuery) -> Vec<usize> {
    let makers = [query.camera_make.as_str(), query.camera_clean_make.as_str()]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .map(normalized)
        .collect::<Vec<_>>();
    let models = [
        query.camera_model.as_str(),
        query.camera_clean_model.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .map(normalized)
    .collect::<Vec<_>>();
    service
        .database
        .cameras
        .iter()
        .enumerate()
        .filter(|(_, camera)| {
            models.iter().any(|model| {
                let prefixes = [
                    query.camera_make.as_str(),
                    query.camera_clean_make.as_str(),
                    camera.maker.as_str(),
                ];
                normalized_name_matches(&camera.model, model, &prefixes)
                    || camera
                        .model_localized
                        .values()
                        .any(|localized| normalized_name_matches(localized, model, &prefixes))
            }) && (makers.is_empty()
                || makers.iter().any(|maker| {
                    normalized(&camera.maker) == *maker
                        || camera
                            .maker_localized
                            .values()
                            .any(|localized| normalized(localized) == *maker)
                }))
        })
        .map(|(index, _)| index)
        .collect()
}

fn record_matches_lens(
    service: &OpticsService,
    record: &ProfileRecord,
    query: &OpticsQuery,
) -> bool {
    let lens = service.lens(record.lens_index);
    let Some(model) = query.lens_model.as_deref() else {
        return false;
    };
    let prefixes = [
        query.lens_make.as_deref().unwrap_or(""),
        lens.maker.as_str(),
    ];
    if !normalized_name_matches(&lens.model, model, &prefixes)
        && !lens
            .model_localized
            .values()
            .any(|localized| normalized_name_matches(localized, model, &prefixes))
    {
        return false;
    }
    query.lens_make.as_deref().is_none_or(|maker| {
        maker.trim().is_empty()
            || normalized(&lens.maker) == normalized(maker)
            || lens
                .maker_localized
                .values()
                .any(|localized| normalized(localized) == normalized(maker))
    })
}

fn focal_in_range(lens: &lensfun::Lens, focal: f32) -> bool {
    let has_range = lens.focal_min > 0.0 && lens.focal_max >= lens.focal_min;
    !has_range || (focal >= lens.focal_min - 0.5 && focal <= lens.focal_max + 0.5)
}

fn normalized_name_matches(candidate: &str, requested: &str, prefixes: &[&str]) -> bool {
    let candidate = normalized(candidate);
    let requested = normalized(requested);
    candidate == requested
        || prefixes
            .iter()
            .filter_map(|prefix| {
                let prefix = normalized(prefix);
                (!prefix.is_empty()).then(|| format!("{prefix} "))
            })
            .any(|prefix| {
                candidate
                    == requested
                        .strip_prefix(&prefix)
                        .unwrap_or(requested.as_str())
                    || requested
                        == candidate
                            .strip_prefix(&prefix)
                            .unwrap_or(candidate.as_str())
            })
}

#[allow(dead_code)]
fn _summary_identity(summary: &LensProfileSummary) -> (&str, &str, &str) {
    (&summary.id, &summary.camera, &summary.lens)
}
