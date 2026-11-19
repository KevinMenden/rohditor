//! Minimal `.npy` reading and writing for C-ordered little-endian `float32`
//! arrays, the only kind the neural-demosaic tooling exchanges with the CLI.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

const MAGIC: &[u8] = b"\x93NUMPY";

/// A dense `float32` array and its shape.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct NpyArray {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

pub(super) fn read_f32(path: &Path) -> Result<NpyArray> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    parse_f32(&bytes).with_context(|| format!("{} is not a float32 .npy array", path.display()))
}

fn parse_f32(bytes: &[u8]) -> Result<NpyArray> {
    if bytes.len() < 10 || &bytes[..6] != MAGIC {
        bail!("missing .npy magic");
    }
    let (header_start, header_len) = match bytes[6] {
        1 => (10, usize::from(u16::from_le_bytes([bytes[8], bytes[9]]))),
        2 | 3 if bytes.len() >= 12 => (
            12,
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
        ),
        version => bail!("unsupported .npy version {version}"),
    };
    let data_start = header_start + header_len;
    let header = std::str::from_utf8(
        bytes
            .get(header_start..data_start)
            .context("truncated header")?,
    )?;
    if !header.contains("'descr': '<f4'") {
        bail!("dtype must be '<f4', header is {header}");
    }
    if !header.contains("'fortran_order': False") {
        bail!("Fortran-ordered arrays are not supported");
    }
    let shape = parse_shape(header)?;
    let count = shape
        .iter()
        .try_fold(1usize, |n, &d| n.checked_mul(d))
        .context("shape overflows")?;
    let payload = &bytes[data_start..];
    if Some(payload.len()) != count.checked_mul(4) {
        bail!(
            "payload has {} bytes, shape {shape:?} needs {}",
            payload.len(),
            count * 4
        );
    }
    let data = payload
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok(NpyArray { shape, data })
}

fn parse_shape(header: &str) -> Result<Vec<usize>> {
    let start = header.find("'shape': (").context("header has no shape")? + "'shape': (".len();
    let end = start + header[start..].find(')').context("unterminated shape")?;
    header[start..end]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().with_context(|| format!("bad dimension {s:?}")))
        .collect()
}

/// Encode a C-ordered `float32` array as a version 1.0 `.npy` file.
pub(super) fn encode_f32(shape: &[usize], data: &[f32]) -> Vec<u8> {
    let dims: Vec<String> = shape.iter().map(ToString::to_string).collect();
    let shape_text = match dims.len() {
        1 => format!("{},", dims[0]),
        _ => dims.join(", "),
    };
    let mut header =
        format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({shape_text}), }}");
    // Pad so the payload starts on a 64-byte boundary; the header ends in '\n'.
    let unpadded = MAGIC.len() + 4 + header.len() + 1;
    header.push_str(&" ".repeat((64 - unpadded % 64) % 64));
    header.push('\n');
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + header.len() + data.len() * 4);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
    bytes.extend_from_slice(header.as_bytes());
    for value in data {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_shapes_and_values() {
        for shape in [vec![3], vec![2, 3], vec![2, 1, 3]] {
            let count: usize = shape.iter().product();
            let data: Vec<f32> = (0..count).map(|i| i as f32 * -0.5 + 0.25).collect();
            let bytes = encode_f32(&shape, &data);
            assert_eq!((bytes.len() - count * 4) % 64, 0, "payload is aligned");
            assert_eq!(parse_f32(&bytes).expect("parse"), NpyArray { shape, data });
        }
    }

    #[test]
    fn rejects_other_dtypes_and_short_payloads() {
        let mut bytes = encode_f32(&[2], &[1.0, 2.0]);
        bytes.pop();
        assert!(parse_f32(&bytes).is_err());
        let text = String::from_utf8_lossy(&encode_f32(&[2], &[1.0, 2.0])).replace("<f4", "<f8");
        assert!(parse_f32(text.as_bytes()).is_err());
    }
}
