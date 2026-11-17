//! Embedded ICC v2 matrix/TRC profiles for the supported display color spaces.

use rohditor_image::{DisplayColorSpace, RgbPrimaries};

use super::align_to_four;

/// Colorants of one RGB space: each primary as D50-adapted XYZ, the form ICC
/// profiles store (white point `wtpt` is D50 in every profile we write).
struct Colorants {
    description: &'static str,
    red: [f64; 3],
    green: [f64; 3],
    blue: [f64; 3],
}

const SRGB_COLORANTS: Colorants = Colorants {
    description: "Rohditor sRGB",
    red: [0.436_074_7, 0.222_504_5, 0.013_932_2],
    green: [0.385_064_9, 0.716_878_6, 0.097_104_5],
    blue: [0.143_080_4, 0.060_616_9, 0.714_173_3],
};

// Display P3 primaries and D65 white, Bradford-adapted to D50.
const DISPLAY_P3_COLORANTS: Colorants = Colorants {
    description: "Rohditor Display P3",
    red: [0.515_119, 0.241_189, -0.001_050],
    green: [0.291_978, 0.692_244, 0.041_879],
    blue: [0.157_103, 0.066_567, 0.784_071],
};

/// The ICC profile to embed alongside pixels in `color_space`.
pub(super) fn icc_profile(color_space: DisplayColorSpace) -> Vec<u8> {
    // The sRGB transfer is the only one so far; Display P3 shares it.
    let colorants = match color_space.primaries {
        RgbPrimaries::Srgb => &SRGB_COLORANTS,
        RgbPrimaries::DisplayP3 => &DISPLAY_P3_COLORANTS,
    };
    matrix_trc_profile(colorants)
}

fn matrix_trc_profile(colorants: &Colorants) -> Vec<u8> {
    let tags = [
        (*b"desc", icc_description(colorants.description)),
        (*b"cprt", icc_text("Public domain color characterization")),
        (*b"wtpt", icc_xyz([0.9642, 1.0, 0.8249])),
        (*b"rXYZ", icc_xyz(colorants.red)),
        (*b"gXYZ", icc_xyz(colorants.green)),
        (*b"bXYZ", icc_xyz(colorants.blue)),
        (*b"rTRC", icc_srgb_curve()),
        (*b"gTRC", icc_srgb_curve()),
        (*b"bTRC", icc_srgb_curve()),
        (
            *b"chad",
            icc_sf32(&[
                1.047_811_2,
                0.022_886_6,
                -0.050_127,
                0.029_542_4,
                0.990_484_4,
                -0.017_049_1,
                -0.009_234_5,
                0.015_043_6,
                0.752_131_6,
            ]),
        ),
    ];
    let table_length = 4 + tags.len() * 12;
    let mut profile = vec![0_u8; 128 + table_length];

    profile[4..8].copy_from_slice(b"Rohd");
    profile[8..12].copy_from_slice(&[0x02, 0x10, 0x00, 0x00]);
    profile[12..16].copy_from_slice(b"mntr");
    profile[16..20].copy_from_slice(b"RGB ");
    profile[20..24].copy_from_slice(b"XYZ ");
    for (index, value) in [2026_u16, 8, 29, 0, 0, 0].into_iter().enumerate() {
        let offset = 24 + index * 2;
        profile[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }
    profile[36..40].copy_from_slice(b"acsp");
    profile[68..72].copy_from_slice(&icc_fixed(0.9642));
    profile[72..76].copy_from_slice(&icc_fixed(1.0));
    profile[76..80].copy_from_slice(&icc_fixed(0.8249));
    profile[80..84].copy_from_slice(b"Rohd");
    profile[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());

    for (index, (signature, data)) in tags.into_iter().enumerate() {
        align_to_four(&mut profile);
        let offset = profile.len();
        let size = data.len();
        profile.extend_from_slice(&data);
        let table_offset = 132 + index * 12;
        profile[table_offset..table_offset + 4].copy_from_slice(&signature);
        profile[table_offset + 4..table_offset + 8].copy_from_slice(&(offset as u32).to_be_bytes());
        profile[table_offset + 8..table_offset + 12].copy_from_slice(&(size as u32).to_be_bytes());
    }
    align_to_four(&mut profile);
    let profile_size = profile.len() as u32;
    profile[0..4].copy_from_slice(&profile_size.to_be_bytes());
    profile
}

fn icc_description(description: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"desc\0\0\0\0");
    data.extend_from_slice(&((description.len() + 1) as u32).to_be_bytes());
    data.extend_from_slice(description.as_bytes());
    data.push(0);
    data.extend_from_slice(&[0; 8]);
    data.extend_from_slice(&[0; 2]);
    data.push(0);
    data.extend_from_slice(&[0; 67]);
    data
}

fn icc_text(value: &str) -> Vec<u8> {
    let mut data = b"text\0\0\0\0".to_vec();
    data.extend_from_slice(value.as_bytes());
    data.push(0);
    data
}

fn icc_xyz(values: [f64; 3]) -> Vec<u8> {
    let mut data = b"XYZ \0\0\0\0".to_vec();
    for value in values {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_sf32(values: &[f64]) -> Vec<u8> {
    let mut data = b"sf32\0\0\0\0".to_vec();
    for &value in values {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_srgb_curve() -> Vec<u8> {
    let mut data = b"para\0\0\0\0\0\x04\0\0".to_vec();
    for value in [
        2.4,
        1.0 / 1.055,
        0.055 / 1.055,
        1.0 / 12.92,
        0.04045,
        0.0,
        0.0,
    ] {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_fixed(value: f64) -> [u8; 4] {
    ((value * 65_536.0).round() as i32).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_has_text(profile: &[u8], text: &str) -> bool {
        profile
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    #[test]
    fn profiles_have_an_icc_header_and_their_own_description() {
        for (space, name) in [
            (DisplayColorSpace::SRGB, "Rohditor sRGB"),
            (DisplayColorSpace::DISPLAY_P3, "Rohditor Display P3"),
        ] {
            let profile = icc_profile(space);
            assert_eq!(
                u32::from_be_bytes(profile[0..4].try_into().expect("profile size")) as usize,
                profile.len()
            );
            assert_eq!(&profile[16..20], b"RGB ");
            assert_eq!(&profile[20..24], b"XYZ ");
            assert_eq!(&profile[36..40], b"acsp");
            assert!(profile_has_text(&profile, name));
        }
        assert!(!profile_has_text(
            &icc_profile(DisplayColorSpace::SRGB),
            "Display P3"
        ));
    }

    #[test]
    fn colorants_sum_to_the_d50_white_point() {
        for colorants in [&SRGB_COLORANTS, &DISPLAY_P3_COLORANTS] {
            for (axis, white) in [0.9642, 1.0, 0.8249].into_iter().enumerate() {
                let sum = colorants.red[axis] + colorants.green[axis] + colorants.blue[axis];
                assert!(
                    (sum - white).abs() < 5.0e-4,
                    "{} axis {axis}: {sum} != {white}",
                    colorants.description
                );
            }
        }
    }
}
