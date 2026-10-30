/// Average of the 5x5 pixel patch around a normalized (0..=1) position in a
/// packed 8-bit image with `channels` interleaved samples per pixel. Returns
/// `None` for an undersized buffer or fewer than three channels.
#[must_use]
pub fn sample_rgb_patch(
    width: usize,
    height: usize,
    channels: usize,
    pixels: &[u8],
    normalized: [f32; 2],
) -> Option<[u8; 3]> {
    if width == 0 || height == 0 || channels < 3 {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(channels)?;
    if pixels.len() < expected {
        return None;
    }
    let center_x = (normalized[0] * (width.saturating_sub(1)) as f32).round() as usize;
    let center_y = (normalized[1] * (height.saturating_sub(1)) as f32).round() as usize;
    let mut totals = [0_u32; 3];
    let mut count = 0_u32;
    for y in center_y.saturating_sub(2)..=(center_y + 2).min(height - 1) {
        for x in center_x.saturating_sub(2)..=(center_x + 2).min(width - 1) {
            let offset = y
                .checked_mul(width)?
                .checked_add(x)?
                .checked_mul(channels)?;
            for (channel, total) in totals.iter_mut().enumerate() {
                *total += u32::from(*pixels.get(offset + channel)?);
            }
            count += 1;
        }
    }
    (count > 0).then(|| totals.map(|total| ((total + count / 2) / count) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_a_bounded_display_patch() {
        let pixels = vec![
            255, 0, 0, 0, 255, 0, // first row
            0, 0, 255, 255, 255, 255, // second row
        ];
        assert_eq!(
            sample_rgb_patch(2, 2, 3, &pixels, [0.0, 0.0]),
            Some([128, 128, 128])
        );
        assert_eq!(sample_rgb_patch(2, 2, 3, &pixels[..9], [0.0, 0.0]), None);
    }
}
