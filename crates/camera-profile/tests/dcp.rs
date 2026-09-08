use std::panic::{AssertUnwindSafe, catch_unwind};

use rohditor_camera_profile::{CalibrationIlluminant, parse_dcp_bytes};

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

struct Tag {
    tag: u16,
    data_type: u16,
    data: Vec<u8>,
}

#[test]
fn parses_little_endian_matrix_profile_and_prefers_d65() {
    let bytes = dcp(Endian::Little, false, true);
    let profile = parse_dcp_bytes(&bytes).expect("matrix DCP should parse");
    assert_eq!(profile.name, "Test Matrix Profile");
    assert_eq!(profile.camera_model, "Sony ILCE-6400");
    assert_eq!(profile.calibrations.len(), 2);
    assert_eq!(
        profile
            .preferred_calibration()
            .map(|value| value.illuminant),
        Some(CalibrationIlluminant::D65)
    );
    assert!(
        profile
            .source_sha256
            .chars()
            .all(|value| value.is_ascii_hexdigit())
    );
    assert_eq!(profile.source_sha256.len(), 64);
}

#[test]
fn parses_big_endian_signed_rationals() {
    let bytes = dcp(Endian::Big, false, false);
    let profile = parse_dcp_bytes(&bytes).expect("big-endian DCP should parse");
    assert_eq!(
        profile.calibrations[0].illuminant,
        CalibrationIlluminant::D50
    );
    assert_eq!(profile.calibrations[0].xyz_to_camera[0][0], -1.0);
}

#[test]
fn rejects_pixel_producing_profile_components() {
    let bytes = dcp(Endian::Little, true, false);
    let error = parse_dcp_bytes(&bytes).expect_err("hue/sat maps are outside the MVP");
    assert!(error.to_string().contains("hue/saturation map"));
}

#[test]
fn malformed_inputs_return_errors_without_panicking() {
    let bytes = dcp(Endian::Little, false, false);
    for length in 0..=bytes.len() {
        let result = catch_unwind(AssertUnwindSafe(|| parse_dcp_bytes(&bytes[..length])));
        assert!(result.is_ok(), "parser panicked at prefix length {length}");
    }
}

#[test]
fn profile_deserialization_enforces_payload_bounds() {
    let oversized_name = serde_json::json!({
        "format_version": 1,
        "source_sha256": "a".repeat(64),
        "name": "x".repeat(257),
        "camera_model": "Test camera",
        "calibrations": [{
            "illuminant": "d65",
            "xyz_to_camera": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        }]
    });
    assert!(
        serde_json::from_value::<rohditor_camera_profile::MatrixCameraProfile>(oversized_name)
            .is_err()
    );

    let too_many_calibrations = serde_json::json!({
        "format_version": 1,
        "source_sha256": "a".repeat(64),
        "name": "Test",
        "camera_model": "Test camera",
        "calibrations": [
            {"illuminant": "d65", "xyz_to_camera": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]},
            {"illuminant": "d50", "xyz_to_camera": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]},
            {"illuminant": "a", "xyz_to_camera": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]}
        ]
    });
    assert!(
        serde_json::from_value::<rohditor_camera_profile::MatrixCameraProfile>(
            too_many_calibrations
        )
        .is_err()
    );
}

fn dcp(endian: Endian, unsupported: bool, include_d65: bool) -> Vec<u8> {
    let mut tags = vec![
        ascii(50_936, "Test Matrix Profile"),
        ascii(50_708, "Sony ILCE-6400"),
        ascii(50_942, "Rohditor test"),
        short(50_778, if include_d65 { 23 } else { 21 }, endian),
        srational_matrix(50_721, endian),
    ];
    if include_d65 {
        tags.push(short(50_779, 21, endian));
        tags.push(srational_matrix(50_722, endian));
        tags.push(srational_matrix(50_965, endian));
    }
    if unsupported {
        tags.push(Tag {
            tag: 50_937,
            data_type: 3,
            data: vec![1, 0, 1, 0, 1, 0],
        });
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(match endian {
        Endian::Little => b"II",
        Endian::Big => b"MM",
    });
    push_u16(&mut bytes, 42, endian);
    push_u32(&mut bytes, 8, endian);
    push_u16(&mut bytes, tags.len() as u16, endian);
    let data_start = 8 + 2 + tags.len() * 12 + 4;
    let mut external = Vec::new();
    for tag in tags {
        push_u16(&mut bytes, tag.tag, endian);
        push_u16(&mut bytes, tag.data_type, endian);
        let element_size = match tag.data_type {
            2 | 7 => 1,
            3 => 2,
            10 => 8,
            _ => panic!("test tag type needs a size"),
        };
        let count = tag.data.len() / element_size;
        push_u32(&mut bytes, count as u32, endian);
        if tag.data.len() <= 4 {
            bytes.extend_from_slice(&tag.data);
            bytes.resize(bytes.len() + (4 - tag.data.len()), 0);
        } else {
            let offset = data_start + external.len();
            push_u32(&mut bytes, offset as u32, endian);
            external.extend_from_slice(&tag.data);
        }
    }
    push_u32(&mut bytes, 0, endian);
    bytes.extend_from_slice(&external);
    bytes
}

fn ascii(tag: u16, value: &str) -> Tag {
    let mut data = value.as_bytes().to_vec();
    data.push(0);
    Tag {
        tag,
        data_type: 2,
        data,
    }
}

fn short(tag: u16, value: u16, endian: Endian) -> Tag {
    let mut data = Vec::new();
    push_u16(&mut data, value, endian);
    Tag {
        tag,
        data_type: 3,
        data,
    }
}

fn srational_matrix(tag: u16, endian: Endian) -> Tag {
    let values = [-1, 0, 0, 0, 1, 0, 0, 0, 1];
    let mut data = Vec::new();
    for value in values {
        push_i32(&mut data, value, endian);
        push_i32(&mut data, 1, endian);
    }
    Tag {
        tag,
        data_type: 10,
        data,
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16, endian: Endian) {
    let encoded = match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    };
    bytes.extend_from_slice(&encoded);
}

fn push_u32(bytes: &mut Vec<u8>, value: u32, endian: Endian) {
    let encoded = match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    };
    bytes.extend_from_slice(&encoded);
}

fn push_i32(bytes: &mut Vec<u8>, value: i32, endian: Endian) {
    let encoded = match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    };
    bytes.extend_from_slice(&encoded);
}
