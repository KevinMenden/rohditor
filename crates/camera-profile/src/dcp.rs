use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::{
    CAMERA_PROFILE_FORMAT_VERSION, CalibrationIlluminant, CameraProfileError, DCP_MAX_FILE_BYTES,
    MatrixCalibration, MatrixCameraProfile,
};

const TAG_DNG_VERSION: u16 = 50_706;
const TAG_DNG_BACKWARD_VERSION: u16 = 50_707;
const TAG_UNIQUE_CAMERA_MODEL: u16 = 50_708;
const TAG_LOCALIZED_CAMERA_MODEL: u16 = 50_709;
const TAG_COLOR_MATRIX_1: u16 = 50_721;
const TAG_COLOR_MATRIX_2: u16 = 50_722;
const TAG_CAMERA_CALIBRATION_1: u16 = 50_723;
const TAG_CAMERA_CALIBRATION_2: u16 = 50_724;
const TAG_REDUCTION_MATRIX_1: u16 = 50_725;
const TAG_REDUCTION_MATRIX_2: u16 = 50_726;
const TAG_ANALOG_BALANCE: u16 = 50_727;
const TAG_AS_SHOT_NEUTRAL: u16 = 50_728;
const TAG_AS_SHOT_WHITE_XY: u16 = 50_729;
const TAG_BASELINE_EXPOSURE: u16 = 50_730;
const TAG_BASELINE_NOISE: u16 = 50_731;
const TAG_BASELINE_SHARPNESS: u16 = 50_732;
const TAG_LINEAR_RESPONSE_LIMIT: u16 = 50_734;
const TAG_DNG_PRIVATE_DATA: u16 = 50_740;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50_778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50_779;
const TAG_AS_SHOT_ICC_PROFILE: u16 = 50_831;
const TAG_AS_SHOT_PRE_PROFILE_MATRIX: u16 = 50_832;
const TAG_CURRENT_ICC_PROFILE: u16 = 50_833;
const TAG_CURRENT_PRE_PROFILE_MATRIX: u16 = 50_834;
const TAG_COLORIMETRIC_REFERENCE: u16 = 50_879;
const TAG_CAMERA_CALIBRATION_SIGNATURE: u16 = 50_931;
const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 50_932;
const TAG_EXTRA_CAMERA_PROFILES: u16 = 50_933;
const TAG_AS_SHOT_PROFILE_NAME: u16 = 50_934;
const TAG_NOISE_REDUCTION_APPLIED: u16 = 50_935;
const TAG_PROFILE_NAME: u16 = 50_936;
const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 50_937;
const TAG_PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50_938;
const TAG_PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50_939;
const TAG_PROFILE_TONE_CURVE: u16 = 50_940;
const TAG_PROFILE_EMBED_POLICY: u16 = 50_941;
const TAG_PROFILE_COPYRIGHT: u16 = 50_942;
const TAG_FORWARD_MATRIX_1: u16 = 50_964;
const TAG_FORWARD_MATRIX_2: u16 = 50_965;
const TAG_PREVIEW_APPLICATION_NAME: u16 = 50_966;
const TAG_PREVIEW_APPLICATION_VERSION: u16 = 50_967;
const TAG_PREVIEW_SETTINGS_NAME: u16 = 50_968;
const TAG_PREVIEW_SETTINGS_DIGEST: u16 = 50_969;
const TAG_PREVIEW_COLOR_SPACE: u16 = 50_970;
const TAG_PREVIEW_DATE_TIME: u16 = 50_971;
const TAG_RAW_IMAGE_DIGEST: u16 = 50_972;
const TAG_ORIGINAL_RAW_FILE_DIGEST: u16 = 50_973;
const TAG_PROFILE_LOOK_TABLE_DIMS: u16 = 50_981;
const TAG_PROFILE_LOOK_TABLE_DATA: u16 = 50_982;
const TAG_PROFILE_HUE_SAT_MAP_ENCODING: u16 = 51_107;
const TAG_PROFILE_LOOK_TABLE_ENCODING: u16 = 51_108;
const TAG_BASELINE_EXPOSURE_OFFSET: u16 = 51_109;
const TAG_DEFAULT_BLACK_RENDER: u16 = 51_110;
const TAG_RAW_TO_PREVIEW_GAIN: u16 = 51_112;
const TAG_OPCODE_LIST_1: u16 = 51_008;
const TAG_OPCODE_LIST_2: u16 = 51_009;
const TAG_OPCODE_LIST_3: u16 = 51_022;

const MAX_IFDS: usize = 64;
const MAX_IFD_ENTRIES: usize = 4_096;
const MAX_STRING_BYTES: usize = 4_096;

/// Parse a classic TIFF DCP from memory.
pub fn parse_dcp_bytes(bytes: &[u8]) -> Result<MatrixCameraProfile, CameraProfileError> {
    if bytes.len() > DCP_MAX_FILE_BYTES {
        return Err(CameraProfileError::FileTooLarge {
            actual: bytes.len(),
            maximum: DCP_MAX_FILE_BYTES,
        });
    }
    let tiff = Tiff::parse(bytes)?;
    let profile_name = tiff.required_ascii(TAG_PROFILE_NAME, "ProfileName")?;
    let camera_model = tiff.required_ascii(TAG_UNIQUE_CAMERA_MODEL, "UniqueCameraModel")?;
    let copyright = tiff.optional_ascii(TAG_PROFILE_COPYRIGHT, "ProfileCopyright")?;
    let calibrations = parse_calibrations(&tiff)?;

    let profile = MatrixCameraProfile {
        format_version: CAMERA_PROFILE_FORMAT_VERSION,
        source_sha256: sha256_hex(bytes),
        name: profile_name,
        camera_model,
        copyright,
        calibrations,
    };
    profile.validate()?;
    Ok(profile)
}

/// Parse a DCP file without retaining its filesystem path.
pub fn parse_dcp_file(path: &Path) -> Result<MatrixCameraProfile, CameraProfileError> {
    let metadata = fs::metadata(path).map_err(|error| CameraProfileError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if metadata.len() > DCP_MAX_FILE_BYTES as u64 {
        return Err(CameraProfileError::FileTooLarge {
            actual: metadata.len().min(usize::MAX as u64) as usize,
            maximum: DCP_MAX_FILE_BYTES,
        });
    }
    let bytes = fs::read(path).map_err(|error| CameraProfileError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    parse_dcp_bytes(&bytes)
}

fn parse_calibrations(tiff: &Tiff<'_>) -> Result<Vec<MatrixCalibration>, CameraProfileError> {
    let mut calibrations = Vec::new();
    for (illuminant_tag, matrix_tag, forward_tag, label) in [
        (
            TAG_CALIBRATION_ILLUMINANT_1,
            TAG_COLOR_MATRIX_1,
            TAG_FORWARD_MATRIX_1,
            "1",
        ),
        (
            TAG_CALIBRATION_ILLUMINANT_2,
            TAG_COLOR_MATRIX_2,
            TAG_FORWARD_MATRIX_2,
            "2",
        ),
    ] {
        let illuminant = tiff.optional_illuminant(illuminant_tag)?;
        let matrix = tiff.has_tag(matrix_tag);
        let forward = tiff.has_tag(forward_tag);
        if illuminant.is_none() && !matrix && !forward {
            continue;
        }
        let illuminant = illuminant.ok_or_else(|| CameraProfileError::InvalidTiff {
            reason: format!(
                "CalibrationIlluminant{label} is required when its matrix tags are present"
            ),
        })?;
        let matrix_name = if label == "1" {
            "ColorMatrix1"
        } else {
            "ColorMatrix2"
        };
        let xyz_to_camera = tiff.required_matrix(matrix_tag, matrix_name)?;
        let forward_camera_to_xyz_d50 = if forward {
            let forward_name = if label == "1" {
                "ForwardMatrix1"
            } else {
                "ForwardMatrix2"
            };
            Some(tiff.required_matrix(forward_tag, forward_name)?)
        } else {
            None
        };
        calibrations.push(MatrixCalibration {
            illuminant,
            xyz_to_camera,
            forward_camera_to_xyz_d50,
        });
    }
    if calibrations.is_empty() {
        return Err(CameraProfileError::MissingTag {
            tag: "ColorMatrix1 or ColorMatrix2",
        });
    }
    Ok(calibrations)
}

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: &[u8]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            Self::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Self::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }

    fn i32(self, bytes: &[u8]) -> i32 {
        match self {
            Self::Little => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Self::Big => i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TiffEntry {
    tag: u16,
    data_type: u16,
    count: u32,
    value_field: [u8; 4],
    byte_len: usize,
}

struct Tiff<'a> {
    bytes: &'a [u8],
    endian: Endian,
    entries: Vec<TiffEntry>,
}

impl<'a> Tiff<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, CameraProfileError> {
        if bytes.len() < 8 {
            return Err(invalid_tiff("header is shorter than eight bytes"));
        }
        let endian = match &bytes[..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(invalid_tiff("byte order marker is neither II nor MM")),
        };
        if endian.u16(&bytes[2..4]) != 42 {
            return Err(invalid_tiff("classic TIFF magic is not 42"));
        }
        let first_ifd = endian.u32(&bytes[4..8]);
        if first_ifd == 0 {
            return Err(invalid_tiff("first IFD offset is zero"));
        }

        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        let mut offset = first_ifd;
        for _ in 0..MAX_IFDS {
            if !visited.insert(offset) {
                return Err(invalid_tiff("IFD chain contains a cycle"));
            }
            let (next, ifd_entries) = parse_ifd_with_next(bytes, endian, offset)?;
            for entry in &ifd_entries {
                if !is_allowed_tag(entry.tag) {
                    return Err(tag_error(entry.tag));
                }
            }
            entries.extend(ifd_entries);
            if next == 0 {
                return Ok(Self {
                    bytes,
                    endian,
                    entries,
                });
            }
            offset = next;
        }
        Err(invalid_tiff("IFD chain exceeds the supported depth"))
    }

    fn has_tag(&self, tag: u16) -> bool {
        self.entries.iter().any(|entry| entry.tag == tag)
    }

    fn entry(&self, tag: u16) -> Result<Option<&TiffEntry>, CameraProfileError> {
        let mut matches = self.entries.iter().filter(|entry| entry.tag == tag);
        let first = matches.next();
        if matches.next().is_some() {
            return Err(invalid_tiff(format!(
                "tag 0x{tag:04x} occurs more than once"
            )));
        }
        Ok(first)
    }

    fn entry_bytes(&self, entry: &TiffEntry) -> Result<Vec<u8>, CameraProfileError> {
        if entry.byte_len <= 4 {
            return Ok(entry.value_field[..entry.byte_len].to_vec());
        }
        let offset = self.endian.u32(&entry.value_field);
        let start = usize_from_u32(offset)?;
        let end = start
            .checked_add(entry.byte_len)
            .ok_or_else(|| invalid_tiff("tag data range overflows"))?;
        self.bytes
            .get(start..end)
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid_tiff("tag data range is outside the file"))
    }

    fn required_ascii(&self, tag: u16, name: &'static str) -> Result<String, CameraProfileError> {
        self.optional_ascii(tag, name)?
            .ok_or(CameraProfileError::MissingTag { tag: name })
    }

    fn optional_ascii(
        &self,
        tag: u16,
        name: &'static str,
    ) -> Result<Option<String>, CameraProfileError> {
        let Some(entry) = self.entry(tag)? else {
            return Ok(None);
        };
        if entry.data_type != 2 || entry.count == 0 || entry.byte_len > MAX_STRING_BYTES {
            return Err(invalid_tag(
                name,
                "expected a bounded non-empty TIFF ASCII value",
            ));
        }
        let bytes = self.entry_bytes(entry)?;
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        if bytes[end..].iter().any(|byte| *byte != 0)
            || bytes[..end]
                .iter()
                .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
        {
            return Err(invalid_tag(name, "contains invalid ASCII text"));
        }
        let value = String::from_utf8(bytes[..end].to_vec())
            .map_err(|_| invalid_tag(name, "is not valid ASCII"))?;
        if value.trim().is_empty() {
            return Err(invalid_tag(name, "must not be empty"));
        }
        Ok(Some(value.trim().to_owned()))
    }

    fn optional_illuminant(
        &self,
        tag: u16,
    ) -> Result<Option<CalibrationIlluminant>, CameraProfileError> {
        let Some(entry) = self.entry(tag)? else {
            return Ok(None);
        };
        if entry.data_type != 3 || entry.count != 1 {
            return Err(invalid_tag(
                illuminant_name(tag),
                "expected one TIFF SHORT value",
            ));
        }
        let bytes = self.entry_bytes(entry)?;
        let value = self.endian.u16(&bytes);
        let illuminant = match value {
            17 => CalibrationIlluminant::StandardLightA,
            21 => CalibrationIlluminant::D50,
            23 => CalibrationIlluminant::D65,
            _ => {
                return Err(CameraProfileError::UnsupportedComponent {
                    component: "calibration illuminant",
                    reason: format!("illuminant code {value} is not D65, D50, or Standard Light A"),
                });
            }
        };
        Ok(Some(illuminant))
    }

    fn required_matrix(
        &self,
        tag: u16,
        name: &'static str,
    ) -> Result<[[f32; 3]; 3], CameraProfileError> {
        let entry = self
            .entry(tag)?
            .ok_or(CameraProfileError::MissingTag { tag: name })?;
        if entry.data_type != 10 || entry.count != 9 {
            return Err(invalid_tag(name, "expected nine TIFF SRATIONAL values"));
        }
        let bytes = self.entry_bytes(entry)?;
        let mut values = [0.0; 9];
        for (index, value) in values.iter_mut().enumerate() {
            let start = index * 8;
            let numerator = self.endian.i32(&bytes[start..start + 4]);
            let denominator = self.endian.i32(&bytes[start + 4..start + 8]);
            if denominator == 0 {
                return Err(invalid_tag(name, "contains a zero denominator"));
            }
            let parsed = f64::from(numerator) / f64::from(denominator);
            if !parsed.is_finite() || !(parsed as f32).is_finite() {
                return Err(invalid_tag(name, "contains a non-finite value"));
            }
            *value = parsed as f32;
        }
        Ok([
            [values[0], values[1], values[2]],
            [values[3], values[4], values[5]],
            [values[6], values[7], values[8]],
        ])
    }
}

fn parse_ifd_with_next(
    bytes: &[u8],
    endian: Endian,
    offset: u32,
) -> Result<(u32, Vec<TiffEntry>), CameraProfileError> {
    let start = usize_from_u32(offset)?;
    let count_end = start
        .checked_add(2)
        .ok_or_else(|| invalid_tiff("IFD count range overflows"))?;
    let count_bytes = bytes
        .get(start..count_end)
        .ok_or_else(|| invalid_tiff("IFD entry count is outside the file"))?;
    let count = endian.u16(count_bytes) as usize;
    if count > MAX_IFD_ENTRIES {
        return Err(invalid_tiff("IFD contains too many entries"));
    }
    let entries_start = start
        .checked_add(2)
        .ok_or_else(|| invalid_tiff("IFD offset overflows"))?;
    let entries_bytes = count
        .checked_mul(12)
        .ok_or_else(|| invalid_tiff("IFD entry range overflows"))?;
    let next_start = entries_start
        .checked_add(entries_bytes)
        .ok_or_else(|| invalid_tiff("IFD next-offset range overflows"))?;
    let next_end = next_start
        .checked_add(4)
        .ok_or_else(|| invalid_tiff("IFD next-offset range overflows"))?;
    let next_bytes = bytes
        .get(next_start..next_end)
        .ok_or_else(|| invalid_tiff("IFD entries are outside the file"))?;
    let next = endian.u32(next_bytes);
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let entry_start = entries_start + index * 12;
        let entry_end = entry_start
            .checked_add(12)
            .ok_or_else(|| invalid_tiff("IFD entry range overflows"))?;
        let entry = bytes
            .get(entry_start..entry_end)
            .ok_or_else(|| invalid_tiff("IFD entry is outside the file"))?;
        let tag = endian.u16(&entry[..2]);
        let data_type = endian.u16(&entry[2..4]);
        let count = endian.u32(&entry[4..8]);
        let element_size = tiff_type_size(data_type)?;
        let byte_len = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(element_size))
            .ok_or_else(|| invalid_tiff("tag data length overflows"))?;
        let mut value_field = [0_u8; 4];
        value_field.copy_from_slice(&entry[8..12]);
        if byte_len > 4 {
            let data_offset = endian.u32(&value_field);
            let data_start = usize_from_u32(data_offset)?;
            let data_end = data_start
                .checked_add(byte_len)
                .ok_or_else(|| invalid_tiff("tag data range overflows"))?;
            if data_end > bytes.len() {
                return Err(invalid_tiff("tag data range is outside the file"));
            }
        }
        entries.push(TiffEntry {
            tag,
            data_type,
            count,
            value_field,
            byte_len,
        });
    }
    Ok((next, entries))
}

fn tiff_type_size(data_type: u16) -> Result<usize, CameraProfileError> {
    match data_type {
        1 | 2 | 6 | 7 => Ok(1),
        3 | 8 => Ok(2),
        4 | 9 | 11 => Ok(4),
        5 | 10 | 12 => Ok(8),
        _ => Err(invalid_tiff(format!(
            "unsupported TIFF field type {data_type}"
        ))),
    }
}

fn is_allowed_tag(tag: u16) -> bool {
    matches!(
        tag,
        TAG_DNG_VERSION
            | TAG_DNG_BACKWARD_VERSION
            | TAG_UNIQUE_CAMERA_MODEL
            | TAG_LOCALIZED_CAMERA_MODEL
            | TAG_COLOR_MATRIX_1
            | TAG_COLOR_MATRIX_2
            | TAG_CALIBRATION_ILLUMINANT_1
            | TAG_CALIBRATION_ILLUMINANT_2
            | TAG_COLORIMETRIC_REFERENCE
            | TAG_CAMERA_CALIBRATION_SIGNATURE
            | TAG_PROFILE_CALIBRATION_SIGNATURE
            | TAG_AS_SHOT_PROFILE_NAME
            | TAG_NOISE_REDUCTION_APPLIED
            | TAG_PROFILE_NAME
            | TAG_PROFILE_EMBED_POLICY
            | TAG_PROFILE_COPYRIGHT
            | TAG_FORWARD_MATRIX_1
            | TAG_FORWARD_MATRIX_2
            | TAG_PREVIEW_APPLICATION_NAME
            | TAG_PREVIEW_APPLICATION_VERSION
            | TAG_PREVIEW_SETTINGS_NAME
            | TAG_PREVIEW_SETTINGS_DIGEST
            | TAG_PREVIEW_COLOR_SPACE
            | TAG_PREVIEW_DATE_TIME
            | TAG_RAW_IMAGE_DIGEST
            | TAG_ORIGINAL_RAW_FILE_DIGEST
    )
}

fn unsupported_component(tag: u16) -> Option<&'static str> {
    match tag {
        TAG_CAMERA_CALIBRATION_1 | TAG_CAMERA_CALIBRATION_2 => Some("CameraCalibration"),
        TAG_REDUCTION_MATRIX_1 | TAG_REDUCTION_MATRIX_2 => Some("ReductionMatrix"),
        TAG_ANALOG_BALANCE => Some("AnalogBalance"),
        TAG_AS_SHOT_NEUTRAL => Some("AsShotNeutral"),
        TAG_AS_SHOT_WHITE_XY => Some("AsShotWhiteXY"),
        TAG_BASELINE_EXPOSURE
        | TAG_BASELINE_NOISE
        | TAG_BASELINE_SHARPNESS
        | TAG_LINEAR_RESPONSE_LIMIT
        | TAG_BASELINE_EXPOSURE_OFFSET
        | TAG_DEFAULT_BLACK_RENDER
        | TAG_RAW_TO_PREVIEW_GAIN => Some("baseline/default exposure data"),
        TAG_DNG_PRIVATE_DATA => Some("DNGPrivateData"),
        TAG_AS_SHOT_ICC_PROFILE
        | TAG_AS_SHOT_PRE_PROFILE_MATRIX
        | TAG_CURRENT_ICC_PROFILE
        | TAG_CURRENT_PRE_PROFILE_MATRIX => Some("ICC profile data"),
        TAG_EXTRA_CAMERA_PROFILES => Some("ExtraCameraProfiles"),
        TAG_PROFILE_HUE_SAT_MAP_DIMS
        | TAG_PROFILE_HUE_SAT_MAP_DATA_1
        | TAG_PROFILE_HUE_SAT_MAP_DATA_2
        | TAG_PROFILE_HUE_SAT_MAP_ENCODING => Some("hue/saturation map"),
        TAG_PROFILE_TONE_CURVE => Some("ProfileToneCurve"),
        TAG_PROFILE_LOOK_TABLE_DIMS
        | TAG_PROFILE_LOOK_TABLE_DATA
        | TAG_PROFILE_LOOK_TABLE_ENCODING => Some("look table"),
        TAG_OPCODE_LIST_1 | TAG_OPCODE_LIST_2 | TAG_OPCODE_LIST_3 => Some("DNG opcode list"),
        _ => None,
    }
}

fn tag_error(tag: u16) -> CameraProfileError {
    if let Some(component) = unsupported_component(tag) {
        CameraProfileError::UnsupportedComponent {
            component,
            reason: format!("tag 0x{tag:04x} is pixel-producing and outside the MVP"),
        }
    } else {
        CameraProfileError::UnsupportedTag {
            tag,
            reason: "unknown tags are rejected to avoid silently changing pixels".to_owned(),
        }
    }
}

fn illuminant_name(tag: u16) -> &'static str {
    match tag {
        TAG_CALIBRATION_ILLUMINANT_1 => "CalibrationIlluminant1",
        TAG_CALIBRATION_ILLUMINANT_2 => "CalibrationIlluminant2",
        _ => "CalibrationIlluminant",
    }
}

fn invalid_tiff(reason: impl Into<String>) -> CameraProfileError {
    CameraProfileError::InvalidTiff {
        reason: reason.into(),
    }
}

fn invalid_tag(name: &'static str, reason: &str) -> CameraProfileError {
    CameraProfileError::InvalidProfile {
        field: name,
        reason: reason.to_owned(),
    }
}

fn usize_from_u32(value: u32) -> Result<usize, CameraProfileError> {
    usize::try_from(value).map_err(|_| invalid_tiff("32-bit offset does not fit this platform"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut schedule = [0_u32; 64];
        for (index, word) in schedule[..16].iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let choice = (e & f) ^ ((!e) & g);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let temp1 = h
                .wrapping_add(sigma1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let temp2 = sigma0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0_u8; 32];
    for (index, word) in state.into_iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}
