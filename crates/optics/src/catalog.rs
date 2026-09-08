use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lensfun::{Camera, Database, Lens, LensType};

use crate::{
    CorrectionComponents, DatabaseProvenance, LensProfileSummary, OpticsError, OpticsQuery,
    ProfileMatch, ProfileRequest,
};

const PLAN_CACHE_CAPACITY: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct ProfileRecord {
    pub(crate) camera_index: usize,
    pub(crate) lens_index: usize,
    pub(crate) summary: LensProfileSummary,
}

/// One immutable, bundled Lensfun snapshot shared by all render jobs.
#[derive(Debug, Clone)]
pub struct OpticsService {
    pub(crate) database: Arc<Database>,
    pub(crate) records: Arc<Vec<ProfileRecord>>,
    provenance: DatabaseProvenance,
    plan_cache: Arc<Mutex<VecDeque<(PlanCacheKey, crate::LensCorrectionPlan)>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanCacheKey {
    camera_make: String,
    camera_model: String,
    camera_clean_make: String,
    camera_clean_model: String,
    lens_make: Option<String>,
    lens_model: Option<String>,
    focal_length_bits: Option<u32>,
    aperture_bits: Option<u32>,
    focus_distance_bits: Option<u32>,
    request: PlanRequestKey,
    requested: CorrectionComponents,
    width: usize,
    height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanRequestKey {
    Automatic,
    Explicit(String),
}

impl PlanCacheKey {
    fn new(
        query: &OpticsQuery,
        request: &ProfileRequest,
        requested: CorrectionComponents,
        width: usize,
        height: usize,
    ) -> Self {
        Self {
            camera_make: query.camera_make.clone(),
            camera_model: query.camera_model.clone(),
            camera_clean_make: query.camera_clean_make.clone(),
            camera_clean_model: query.camera_clean_model.clone(),
            lens_make: query.lens_make.clone(),
            lens_model: query.lens_model.clone(),
            focal_length_bits: query.focal_length_mm.map(f32::to_bits),
            aperture_bits: query.aperture_f_number.map(f32::to_bits),
            focus_distance_bits: query.focus_distance_m.map(f32::to_bits),
            request: match request {
                ProfileRequest::Automatic => PlanRequestKey::Automatic,
                ProfileRequest::Explicit { profile_id } => {
                    PlanRequestKey::Explicit(profile_id.clone())
                }
            },
            requested,
            width,
            height,
        }
    }
}

impl OpticsService {
    /// Load the exact Lensfun database embedded by the pinned dependency.
    pub fn load_bundled() -> Result<Self, OpticsError> {
        let database = Database::load_bundled().map_err(|error| OpticsError::Database {
            reason: error.to_string(),
        })?;
        let service = Self::from_database(database, "lensfun bundled database", "lensfun-0.7.0");
        tracing::info!(
            snapshot = %service.provenance.snapshot,
            fingerprint = service.provenance.fingerprint,
            profiles = service.records.len(),
            "loaded bundled Lensfun database"
        );
        Ok(service)
    }

    /// Build a service from an in-memory Lensfun XML document.
    ///
    /// This is useful for deterministic tests and does not expose a Lensfun
    /// database type to callers.
    pub fn from_xml(xml: &str) -> Result<Self, OpticsError> {
        let mut database = Database::new();
        database
            .load_str(xml)
            .map_err(|error| OpticsError::Database {
                reason: error.to_string(),
            })?;
        Ok(Self::from_database(
            database,
            "in-memory Lensfun XML",
            "custom-xml",
        ))
    }

    fn from_database(database: Database, source: &str, snapshot: &str) -> Self {
        let provenance = DatabaseProvenance {
            source: source.to_owned(),
            snapshot: snapshot.to_owned(),
            fingerprint: database_fingerprint(&database),
        };
        let records = build_records(&database);
        Self {
            database: Arc::new(database),
            records: Arc::new(records),
            provenance,
            plan_cache: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    #[must_use]
    pub fn database_provenance(&self) -> &DatabaseProvenance {
        &self.provenance
    }

    /// Return all deterministic profile summaries in the bundled snapshot.
    #[must_use]
    pub fn profiles(&self) -> Vec<LensProfileSummary> {
        self.records
            .iter()
            .map(|record| record.summary.clone())
            .collect()
    }

    /// Resolve camera/lens metadata without constructing a pixel plan.
    #[must_use]
    pub fn profile_match(&self, query: &OpticsQuery) -> ProfileMatch {
        crate::matching::profile_match(self, query)
    }

    /// Return a bounded, deterministic list of camera-compatible profiles for
    /// manual selection. Capture lens metadata narrows the list when it is
    /// usable, but never removes the manual escape hatch for incorrect EXIF.
    #[must_use]
    pub fn manual_candidates(&self, query: &OpticsQuery) -> Vec<LensProfileSummary> {
        crate::matching::manual_candidates(self, query)
    }

    /// Build the immutable calibration plan for one developed raster.
    pub fn resolve_plan(
        &self,
        query: &OpticsQuery,
        request: ProfileRequest,
        requested: CorrectionComponents,
        width: usize,
        height: usize,
    ) -> Result<crate::LensCorrectionPlan, OpticsError> {
        let key = PlanCacheKey::new(query, &request, requested, width, height);
        if let Some(plan) = self.cached_plan(&key) {
            tracing::debug!(profile = %plan.profile.id, "reused cached optics plan");
            return Ok(plan);
        }
        let plan = crate::plan::build_plan(self, query, request, requested, width, height)?;
        tracing::debug!(
            profile = %plan.profile.id,
            applied = ?plan.applied,
            "resolved optics plan"
        );
        self.cache_plan(key, plan.clone());
        Ok(plan)
    }

    fn cached_plan(&self, key: &PlanCacheKey) -> Option<crate::LensCorrectionPlan> {
        let mut cache = self
            .plan_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = cache.iter().position(|(candidate, _)| candidate == key)?;
        let (cached_key, plan) = cache.remove(index)?;
        cache.push_front((cached_key, plan.clone()));
        Some(plan)
    }

    fn cache_plan(&self, key: PlanCacheKey, plan: crate::LensCorrectionPlan) {
        let mut cache = self
            .plan_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = cache.iter().position(|(candidate, _)| candidate == &key) {
            let _ = cache.remove(index);
        }
        cache.push_front((key, plan));
        if cache.len() > PLAN_CACHE_CAPACITY {
            let _ = cache.pop_back();
        }
    }

    pub(crate) fn camera(&self, index: usize) -> &Camera {
        &self.database.cameras[index]
    }

    pub(crate) fn lens(&self, index: usize) -> &Lens {
        &self.database.lenses[index]
    }
}

fn build_records(database: &Database) -> Vec<ProfileRecord> {
    let mut candidates = Vec::new();
    for (camera_index, camera) in database.cameras.iter().enumerate() {
        for (lens_index, lens) in database.lenses.iter().enumerate() {
            let Some(mount) = compatible_mount_without_database(database, camera, lens) else {
                continue;
            };
            if !crop_compatible_without_database(camera, lens)
                || lens.lens_type != LensType::Rectilinear
            {
                continue;
            }
            candidates.push((
                normalized(&camera.maker),
                normalized(&camera.model),
                normalized(&lens.maker),
                normalized(&lens.model),
                normalized(&mount),
                camera_index,
                lens_index,
            ));
        }
    }
    candidates.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.4.cmp(&right.4))
    });

    candidates
        .into_iter()
        .map(
            |(
                camera_maker,
                camera_model,
                lens_maker,
                lens_model,
                mount,
                camera_index,
                lens_index,
            )| {
                let camera = &database.cameras[camera_index];
                let lens = &database.lenses[lens_index];
                let available = available_components(lens);
                // The ID is derived only from canonical profile facts. It must
                // remain stable when the database vector is reordered or its
                // calibration coefficients are revised; an ordinal would
                // make saved recipes silently refer to another lens.
                let id = stable_profile_id(
                    &camera_maker,
                    &camera_model,
                    &lens_maker,
                    &lens_model,
                    &mount,
                );
                ProfileRecord {
                    camera_index,
                    lens_index,
                    summary: LensProfileSummary {
                        id,
                        camera: format!("{} {}", camera.maker, camera.model),
                        lens: format!(
                            "{} {}",
                            display_name(&lens.maker, &lens.maker_localized),
                            display_name(&lens.model, &lens.model_localized)
                        ),
                        mount,
                        available,
                    },
                }
            },
        )
        .collect()
}

fn stable_profile_id(
    camera_maker: &str,
    camera_model: &str,
    lens_maker: &str,
    lens_model: &str,
    mount: &str,
) -> String {
    // Keep calibration out of the logical ID. A reviewed database update may
    // change coefficients while retaining the same camera/lens identity; the
    // plan and database fingerprints then invalidate pixel caches separately.
    format!("lensfun:{camera_maker}:{camera_model}:{lens_maker}:{lens_model}:{mount}")
}

fn available_components(lens: &Lens) -> CorrectionComponents {
    CorrectionComponents {
        distortion: lens
            .calib_distortion
            .iter()
            .any(|entry| !matches!(entry.model, lensfun::DistortionModel::None)),
        vignetting: lens
            .calib_vignetting
            .iter()
            .any(|entry| !matches!(entry.model, lensfun::VignettingModel::None)),
        chromatic_aberration: lens
            .calib_tca
            .iter()
            .any(|entry| !matches!(entry.model, lensfun::TcaModel::None)),
    }
}

fn compatible_mount_without_database(
    database: &Database,
    camera: &Camera,
    lens: &Lens,
) -> Option<String> {
    lens.mounts
        .iter()
        .find(|mount| *mount == &camera.mount)
        .cloned()
        .or_else(|| {
            database
                .mounts
                .iter()
                .find(|entry| entry.name == camera.mount)
                .and_then(|entry| {
                    lens.mounts
                        .iter()
                        .find(|mount| entry.compat.iter().any(|compat| compat == *mount))
                        .cloned()
                })
        })
}

fn display_name(canonical: &str, localized: &std::collections::BTreeMap<String, String>) -> String {
    localized
        .get("en")
        .or_else(|| localized.values().next())
        .cloned()
        .unwrap_or_else(|| canonical.to_owned())
}

fn crop_compatible_without_database(camera: &Camera, lens: &Lens) -> bool {
    camera.crop_factor.is_finite()
        && lens.crop_factor.is_finite()
        && camera.crop_factor > 0.0
        && lens.crop_factor > 0.0
        && camera.crop_factor >= lens.crop_factor * 0.96
}

pub(crate) fn normalized(value: &str) -> String {
    let mut output = String::new();
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with(' ') {
            output.push(' ');
        }
    }
    let normalized = output.trim();
    let mut compacted = String::with_capacity(normalized.len());
    let mut characters = normalized.chars().peekable();
    while let Some(character) = characters.next() {
        // Lens databases commonly spell an f-number as either `f/2.8` or
        // `f2.8`. Keep both forms in the same exact-match token without
        // broadening the matcher into fuzzy search.
        if character == ' '
            && (compacted == "f" || compacted.ends_with(" f"))
            && characters.peek().is_some_and(|next| next.is_ascii_digit())
        {
            continue;
        }
        compacted.push(character);
    }
    compacted
}

fn database_fingerprint(database: &Database) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for mount in &database.mounts {
        hash_text(&mut hash, &mount.name);
        for compatible in &mount.compat {
            hash_text(&mut hash, compatible);
        }
    }
    for camera in &database.cameras {
        hash_text(&mut hash, &camera.maker);
        hash_text(&mut hash, &camera.model);
        hash_text(&mut hash, &camera.mount);
        hash_f32(&mut hash, camera.crop_factor);
    }
    for lens in &database.lenses {
        hash_text(&mut hash, &lens.maker);
        hash_text(&mut hash, &lens.model);
        for mount in &lens.mounts {
            hash_text(&mut hash, mount);
        }
        hash_f32(&mut hash, lens.focal_min);
        hash_f32(&mut hash, lens.focal_max);
        hash_f32(&mut hash, lens.aperture_min);
        hash_f32(&mut hash, lens.aperture_max);
        for calibration in &lens.calib_distortion {
            hash_f32(&mut hash, calibration.focal);
            hash_text(&mut hash, &format!("{:?}", calibration.model));
            hash_f32(&mut hash, calibration.real_focal.unwrap_or_default());
        }
        for calibration in &lens.calib_tca {
            hash_f32(&mut hash, calibration.focal);
            hash_text(&mut hash, &format!("{:?}", calibration.model));
        }
        for calibration in &lens.calib_vignetting {
            hash_f32(&mut hash, calibration.focal);
            hash_f32(&mut hash, calibration.aperture);
            hash_f32(&mut hash, calibration.distance);
            hash_text(&mut hash, &format!("{:?}", calibration.model));
        }
    }
    hash
}

pub(crate) fn hash_text(hash: &mut u64, text: &str) {
    for byte in text.as_bytes() {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x100000001b3);
}

pub(crate) fn hash_f32(hash: &mut u64, value: f32) {
    hash_text(hash, &value.to_bits().to_string());
}
