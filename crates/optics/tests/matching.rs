use rohditor_image::{LinearRgbImage, LinearRgbSpace};
use rohditor_optics::{
    CorrectionComponents, OpticsError, OpticsQuery, OpticsService, ProfileMatch, ProfileRequest,
};

const FIXTURE_XML: &str = r#"
<lensdatabase version="2">
  <camera>
    <maker>Fixture Camera</maker>
    <model>Body One</model>
    <mount>Fixture Mount</mount>
    <cropfactor>1</cropfactor>
  </camera>
  <camera>
    <maker>Fixture Camera</maker>
    <model>Body Two</model>
    <mount>Fixture Mount</mount>
    <cropfactor>1</cropfactor>
  </camera>
  <lens>
    <maker>Fixture Lens</maker>
    <model>Prime 35</model>
    <mount>Fixture Mount</mount>
    <cropfactor>1</cropfactor>
    <focal min="24" max="70" />
    <aperture min="2.8" max="22" />
    <calibration>
      <distortion model="poly3" focal="35" k1="0.01" />
      <tca model="linear" focal="35" kr="1.001" kb="0.999" />
      <vignetting model="pa" focal="35" aperture="2.8" distance="1000" k1="0.1" k2="0.01" k3="0.001" />
    </calibration>
  </lens>
</lensdatabase>
"#;

fn sony_tamron_query() -> OpticsQuery {
    OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "Alpha 6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("17-70mm F2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(2.8),
        focus_distance_m: None,
    }
}

#[test]
fn bundled_database_resolves_localized_camera_and_lens_names() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let query = sony_tamron_query();
    let ProfileMatch::Unique(summary) = service.profile_match(&query) else {
        panic!("Sony A6400 and Tamron 17-70 should resolve uniquely");
    };
    assert!(summary.id.starts_with("lensfun:sony:ilce 6400:tamron:"));
    assert_eq!(
        summary.available,
        CorrectionComponents {
            distortion: true,
            vignetting: true,
            chromatic_aberration: true,
        }
    );
    assert!(summary.lens.contains("17-70mm"));
}

#[test]
fn matching_accepts_common_maker_prefixes_without_fuzzy_auto_selection() {
    let service = OpticsService::from_xml(FIXTURE_XML).expect("fixture database should load");
    let mut query = fixture_query("Body One");
    query.camera_model = "Fixture Camera Body One".to_owned();
    query.camera_clean_model = query.camera_model.clone();
    query.lens_model = Some("Fixture Lens Prime 35".to_owned());

    let ProfileMatch::Unique(summary) = service.profile_match(&query) else {
        panic!("maker-prefixed names should still resolve exactly");
    };
    assert!(summary.camera.contains("Body One"));
}

#[test]
fn plan_uses_infinity_fallback_and_preserves_camera_native_samples() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let query = sony_tamron_query();
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
            16,
            12,
        )
        .expect("supported profile should produce a plan");
    let scale = plan.scale();
    assert!(scale >= 1.0);
    assert!(plan.provenance().used_infinity_distance_fallback);

    let pixels = (0..16 * 12 * 3)
        .map(|index| (index as f32 - 100.0) / 100.0)
        .collect();
    let image = LinearRgbImage::new(16, 12, 16 * 3, LinearRgbSpace::CameraNative, pixels)
        .expect("test image dimensions should be valid");
    let result = plan.apply(image).expect("correction should complete");
    assert_eq!((result.image.width(), result.image.height()), (16, 12));
    assert_eq!(result.provenance.scale, scale);
    assert!(result.image.data().iter().all(|value| value.is_finite()));
}

#[test]
fn missing_focus_does_not_block_distortion_and_tca() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let mut query = sony_tamron_query();
    query.aperture_f_number = None;
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
            16,
            12,
        )
        .expect("missing aperture should only disable vignetting");
    assert!(plan.provenance().applied.distortion);
    assert!(plan.provenance().applied.chromatic_aberration);
    assert!(!plan.provenance().applied.vignetting);
    assert!(!plan.provenance().used_infinity_distance_fallback);
}

#[test]
fn missing_lens_metadata_still_exposes_camera_candidates() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let mut query = sony_tamron_query();
    query.lens_make = None;
    query.lens_model = None;

    assert!(matches!(
        service.profile_match(&query),
        ProfileMatch::MissingMetadata { .. }
    ));
    let candidates = service.manual_candidates(&query);
    assert!(!candidates.is_empty());
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.camera.contains("6400"))
    );
}

#[test]
fn tiny_xml_database_is_owned_and_reports_invalid_documents() {
    let service = OpticsService::from_xml(FIXTURE_XML).expect("fixture database should load");
    assert_eq!(service.profiles().len(), 2);
    let query = fixture_query("Body One");
    let ProfileMatch::Unique(summary) = service.profile_match(&query) else {
        panic!("fixture camera and lens should resolve uniquely");
    };
    assert_eq!(
        summary.available,
        CorrectionComponents {
            distortion: true,
            vignetting: true,
            chromatic_aberration: true,
        }
    );

    let malformed = OpticsService::from_xml("<lensdatabase version=\"2\">");
    assert!(matches!(malformed, Err(OpticsError::Database { .. })));
    let invalid_entry = OpticsService::from_xml(
        "<lensdatabase version=\"2\"><lens><model>Missing mount</model></lens></lensdatabase>",
    );
    assert!(matches!(invalid_entry, Err(OpticsError::Database { .. })));
}

#[test]
fn profile_id_survives_calibration_updates_while_database_fingerprint_changes() {
    let original = OpticsService::from_xml(FIXTURE_XML).expect("fixture database should load");
    let updated_xml = FIXTURE_XML.replace("k1=\"0.01\"", "k1=\"0.02\"");
    let updated = OpticsService::from_xml(&updated_xml).expect("updated fixture should load");
    let query = fixture_query("Body One");

    let ProfileMatch::Unique(original_profile) = original.profile_match(&query) else {
        panic!("original fixture should resolve uniquely");
    };
    let ProfileMatch::Unique(updated_profile) = updated.profile_match(&query) else {
        panic!("updated fixture should resolve uniquely");
    };
    assert_eq!(original_profile.id, updated_profile.id);
    assert_ne!(
        original.database_provenance().fingerprint,
        updated.database_provenance().fingerprint
    );
}

#[test]
fn explicit_profile_selection_allows_missing_lens_metadata_but_checks_camera_and_focal() {
    let service = OpticsService::from_xml(FIXTURE_XML).expect("fixture database should load");
    let query = fixture_query("Body One");
    let ProfileMatch::Unique(summary) = service.profile_match(&query) else {
        panic!("fixture camera and lens should resolve uniquely");
    };

    let mut missing_lens = query.clone();
    missing_lens.lens_make = None;
    missing_lens.lens_model = None;
    let plan = service
        .resolve_plan(
            &missing_lens,
            ProfileRequest::Explicit {
                profile_id: summary.id.clone(),
            },
            CorrectionComponents::none(),
            8,
            8,
        )
        .expect("manual profile selection should recover missing lens metadata");
    assert_eq!(plan.provenance().profile.id, summary.id);

    let mut incompatible_camera = missing_lens.clone();
    incompatible_camera.camera_model = "Body Two".to_owned();
    incompatible_camera.camera_clean_model = "Body Two".to_owned();
    assert!(matches!(
        service.resolve_plan(
            &incompatible_camera,
            ProfileRequest::Explicit {
                profile_id: summary.id.clone(),
            },
            CorrectionComponents::none(),
            8,
            8,
        ),
        Err(OpticsError::IncompatibleProfile { .. })
    ));

    let mut outside_range = query;
    outside_range.focal_length_mm = Some(70.6);
    assert!(matches!(
        service.profile_match(&outside_range),
        ProfileMatch::LensNotFound
    ));
}

fn fixture_query(camera_model: &str) -> OpticsQuery {
    OpticsQuery {
        camera_make: "Fixture Camera".to_owned(),
        camera_model: camera_model.to_owned(),
        camera_clean_make: "Fixture Camera".to_owned(),
        camera_clean_model: camera_model.to_owned(),
        lens_make: Some("Fixture Lens".to_owned()),
        lens_model: Some("Prime 35".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(2.8),
        focus_distance_m: None,
    }
}
