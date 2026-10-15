//! Minimal linear-CFA DNG writer.
//!
//! The file is an uncompressed 16-bit single-strip DNG with black level 0
//! and white level 65535, so every tool reads exactly the samples we wrote.
//! The colour matrix and as-shot neutral are built from white-balance gains
//! so that, after a tool applies its white balance, the output is
//! `gains * camera` in linear sRGB. With unit gains the camera is linear sRGB.

use std::path::Path;

use rohditor_image::{BayerPattern, CfaColor, MosaicImage};

use crate::ReferenceError;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_BYTE: u16 = 1;
const TYPE_SRATIONAL: u16 = 10;

pub const WHITE_LEVEL: u16 = u16::MAX;

/// XYZ (D65) to linear sRGB, row-major.
const XYZ_D65_TO_SRGB: [f64; 9] = [
    3.240_454_2,
    -1.537_138_5,
    -0.498_531_4,
    -0.969_266,
    1.876_010_8,
    0.041_556,
    0.055_643_4,
    -0.204_025_9,
    1.057_225_2,
];

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

fn shorts(tag: u16, values: &[u16]) -> Entry {
    Entry {
        tag,
        kind: TYPE_SHORT,
        count: values.len() as u32,
        data: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
    }
}

fn longs(tag: u16, values: &[u32]) -> Entry {
    Entry {
        tag,
        kind: TYPE_LONG,
        count: values.len() as u32,
        data: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
    }
}

fn bytes(tag: u16, values: &[u8]) -> Entry {
    Entry {
        tag,
        kind: TYPE_BYTE,
        count: values.len() as u32,
        data: values.to_vec(),
    }
}

fn ascii(tag: u16, text: &str) -> Entry {
    let mut data = text.as_bytes().to_vec();
    data.push(0);
    Entry {
        tag,
        kind: TYPE_ASCII,
        count: data.len() as u32,
        data,
    }
}

/// Fixed-denominator (1e6) signed rationals.
fn srationals(tag: u16, values: &[f64]) -> Entry {
    Entry {
        tag,
        kind: TYPE_SRATIONAL,
        count: values.len() as u32,
        data: values
            .iter()
            .flat_map(|v| {
                let numerator = (v * 1e6).round() as i32;
                [numerator.to_le_bytes(), 1_000_000_i32.to_le_bytes()].concat()
            })
            .collect(),
    }
}

fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
    Entry {
        tag,
        kind: TYPE_RATIONAL,
        count: values.len() as u32,
        data: values
            .iter()
            .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
            .collect(),
    }
}

/// AsShotNeutral: the camera response to white, normalized so green is 1.
fn neutral(gains: [f64; 3]) -> [(u32, u32); 3] {
    gains.map(|gain| ((gains[1] / gain * 1e6).round() as u32, 1_000_000))
}

fn cfa_codes(pattern: BayerPattern) -> [u8; 4] {
    let code = |x, y| match pattern.color_at(x, y) {
        CfaColor::Red => 0,
        CfaColor::Green => 1,
        CfaColor::Blue => 2,
    };
    [code(0, 0), code(1, 0), code(0, 1), code(1, 1)]
}

/// `XYZ_D65_TO_SRGB` with row `i` divided by `gains[i]`: camera RGB that
/// becomes linear sRGB once multiplied by `gains`.
fn xyz_to_camera(gains: [f64; 3]) -> [f64; 9] {
    let mut matrix = XYZ_D65_TO_SRGB;
    for (index, value) in matrix.iter_mut().enumerate() {
        *value /= gains[index / 3];
    }
    matrix
}

/// Write `mosaic` (normalized 0..=1) as a 16-bit linear DNG whose as-shot
/// white balance multiplies camera RGB by `gains` (green need not be 1).
pub fn write_mosaic_dng(
    path: &Path,
    mosaic: &MosaicImage<f32>,
    gains: [f64; 3],
) -> Result<(), ReferenceError> {
    let (width, height) = (mosaic.width(), mosaic.height());
    let mut pixels = Vec::with_capacity(width * height * 2);
    for y in 0..height {
        for x in 0..width {
            let value = (mosaic.sample(x, y).clamp(0.0, 1.0) * f32::from(WHITE_LEVEL)).round();
            pixels.extend_from_slice(&(value as u16).to_le_bytes());
        }
    }
    let strip_len = u32::try_from(pixels.len())
        .map_err(|_| ReferenceError::Other("mosaic too large for a classic DNG".into()))?;

    // Pixel data sits right after the 8-byte header; the IFD follows it.
    let mut entries = vec![
        longs(254, &[0]),
        longs(256, &[width as u32]),
        longs(257, &[height as u32]),
        shorts(258, &[16]),
        shorts(259, &[1]),
        shorts(262, &[32803]),
        longs(273, &[8]),
        shorts(277, &[1]),
        longs(278, &[height as u32]),
        longs(279, &[strip_len]),
        shorts(284, &[1]),
        shorts(33421, &[2, 2]),
        bytes(33422, &cfa_codes(mosaic.pattern())),
        bytes(50706, &[1, 4, 0, 0]),
        bytes(50707, &[1, 1, 0, 0]),
        ascii(50708, "Rohditor Reference Mosaic"),
        shorts(50711, &[1]),
        longs(50714, &[0]),
        shorts(50717, &[WHITE_LEVEL]),
        srationals(50721, &xyz_to_camera(gains)),
        rationals(50728, &neutral(gains)),
        shorts(50778, &[21]),
    ];
    entries.sort_by_key(|entry| entry.tag);

    let ifd_offset = 8 + pixels.len() + pixels.len() % 2;
    let ifd_len = 2 + entries.len() * 12 + 4;
    let mut extra_offset = ifd_offset + ifd_len;
    let mut ifd = Vec::new();
    let mut extra = Vec::new();
    ifd.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in &entries {
        ifd.extend_from_slice(&entry.tag.to_le_bytes());
        ifd.extend_from_slice(&entry.kind.to_le_bytes());
        ifd.extend_from_slice(&entry.count.to_le_bytes());
        if entry.data.len() <= 4 {
            let mut inline = [0_u8; 4];
            inline[..entry.data.len()].copy_from_slice(&entry.data);
            ifd.extend_from_slice(&inline);
        } else {
            ifd.extend_from_slice(&(extra_offset as u32).to_le_bytes());
            extra.extend_from_slice(&entry.data);
            if entry.data.len() % 2 == 1 {
                extra.push(0);
            }
            extra_offset = ifd_offset + ifd_len + extra.len();
        }
    }
    ifd.extend_from_slice(&0_u32.to_le_bytes());

    let mut file = Vec::new();
    file.extend_from_slice(b"II*\0");
    file.extend_from_slice(&(ifd_offset as u32).to_le_bytes());
    file.extend_from_slice(&pixels);
    if pixels.len() % 2 == 1 {
        file.push(0);
    }
    file.extend_from_slice(&ifd);
    file.extend_from_slice(&extra);
    std::fs::write(path, file)?;
    Ok(())
}
