//! Round trip: SDR base + gain map at full headroom must reproduce the HDR rendition.

#![allow(clippy::unwrap_used)]

use rohditor_gainmap::{GainMapParams, LinearRgbView, apply_gain_map, compute_gain_map, iso21496};

/// Quantisation alone costs at most half an 8-bit step of a 3-stop map: 3/255/2 = 0.59 % of
/// the value. The half-resolution map cannot follow a gradient exactly (bilinear upsampling
/// of block means, clamped at the borders), which is why smooth scenes get a wider band than
/// uniform ones. A small absolute floor covers near-black, where the offsets dominate.
const UNIFORM_TOLERANCE: f32 = 0.01;
///
/// For the test ramp (3 stops over 76 px = 0.039 stops/px) the block mean at a border lags the
/// corner pixel by about one pixel of gradient, 2.7 %; quantisation (and gamma) add the rest.
const RAMP_TOLERANCE: f32 = 0.04;
const ABSOLUTE_TOLERANCE: f32 = 0.002;

/// Test scene whose SDR colour varies by position (asymmetric in x and y). The HDR rendition is
/// the SDR scaled by `stops(x, y)`, so hue is preserved, which a luminance-only map requires.
fn scene(width: usize, height: usize, stops: impl Fn(usize, usize) -> f32) -> (Vec<f32>, Vec<f32>) {
    let mut sdr = Vec::new();
    let mut hdr = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let rgb = [
                0.05 + 0.9 * x as f32 / (width - 1) as f32,
                0.1 + 0.6 * y as f32 / (height - 1) as f32,
                0.3,
            ];
            let scale = stops(x, y).exp2();
            sdr.extend_from_slice(&rgb);
            hdr.extend(rgb.iter().map(|v| v * scale));
        }
    }
    (sdr, hdr)
}

/// Like [`scene`] but every pixel is grey, the case where a luminance map with offsets is exact.
fn grey_scene(
    width: usize,
    height: usize,
    stops: impl Fn(usize, usize) -> f32,
) -> (Vec<f32>, Vec<f32>) {
    let mut sdr = Vec::new();
    let mut hdr = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let v = 0.02 + 0.9 * (x * height + y) as f32 / (width * height) as f32;
            sdr.extend_from_slice(&[v; 3]);
            hdr.extend_from_slice(&[v * stops(x, y).exp2(); 3]);
        }
    }
    (sdr, hdr)
}

/// With non-zero offsets a single luminance gain only reproduces *luminance* exactly: a dark
/// channel of a saturated pixel overshoots by the offset terms (`(c + o) * g - o` with
/// `g` computed from the luminance). That is inherent to the format, so coloured scenes are
/// checked with zero offsets and the default offsets are checked on grey scenes.
fn zero_offset_params() -> GainMapParams {
    GainMapParams {
        offset_sdr: 0.0,
        offset_hdr: 0.0,
        ..GainMapParams::default()
    }
}

/// Smooth boost from 0 stops (top left) to 3 stops (bottom right).
fn ramp(width: usize, height: usize) -> impl Fn(usize, usize) -> f32 {
    move |x, y| 3.0 * (x + y) as f32 / (width + height - 2) as f32
}

fn assert_close(got: &[f32], want: &[f32], relative: f32) {
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let allowed = ABSOLUTE_TOLERANCE + relative * w;
        assert!(
            (g - w).abs() <= allowed,
            "sample {i}: restored {g}, expected {w} (+-{allowed})"
        );
    }
}

/// Computes the map for `sdr`/`hdr`, decodes it at `headroom` and returns the result.
fn round_trip(
    (width, height): (usize, usize),
    sdr: &[f32],
    hdr: &[f32],
    params: &GainMapParams,
    headroom: f32,
) -> Vec<f32> {
    let sdr_view = LinearRgbView::new(width, height, sdr).unwrap();
    let hdr_view = LinearRgbView::new(width, height, hdr).unwrap();
    let (map, metadata) = compute_gain_map(&sdr_view, &hdr_view, params, &|| false).unwrap();
    assert_eq!(
        (map.width(), map.height()),
        (width.div_ceil(2), height.div_ceil(2))
    );
    apply_gain_map(&sdr_view, &map, &metadata, headroom).unwrap()
}

#[test]
fn small_asymmetric_image_round_trips() {
    // 7x5: odd sizes, so the last map row/column covers a partial block.
    let (sdr, hdr) = scene(7, 5, |_, _| 2.0);
    let params = zero_offset_params();
    let restored = round_trip((7, 5), &sdr, &hdr, &params, params.hdr_headroom_log2);
    assert_close(&restored, &hdr, UNIFORM_TOLERANCE);
}

#[test]
fn smooth_ramp_round_trips_at_full_headroom() {
    let (sdr, hdr) = scene(48, 30, ramp(48, 30));
    let params = zero_offset_params();
    let restored = round_trip((48, 30), &sdr, &hdr, &params, params.hdr_headroom_log2);
    assert_close(&restored, &hdr, RAMP_TOLERANCE);
}

#[test]
fn default_offsets_are_exact_on_grey() {
    let (sdr, hdr) = grey_scene(48, 30, ramp(48, 30));
    let params = GainMapParams::default();
    let restored = round_trip((48, 30), &sdr, &hdr, &params, params.hdr_headroom_log2);
    assert_close(&restored, &hdr, RAMP_TOLERANCE);
}

#[test]
fn no_headroom_reproduces_the_base() {
    let (sdr, hdr) = scene(48, 30, ramp(48, 30));
    let restored = round_trip((48, 30), &sdr, &hdr, &GainMapParams::default(), 0.0);
    for (got, want) in restored.iter().zip(&sdr) {
        assert!((got - want).abs() < 1e-5, "{got} vs {want}");
    }
}

#[test]
fn half_headroom_applies_half_the_stops() {
    // A 2-stop rendition with 3 stops of headroom on a display with 1.5 stops gets
    // 2 * (1.5 / 3) = 1 stop of boost: twice the base.
    let (sdr, hdr) = scene(7, 5, |_, _| 2.0);
    let restored = round_trip((7, 5), &sdr, &hdr, &zero_offset_params(), 1.5);
    let doubled: Vec<f32> = sdr.iter().map(|v| v * 2.0).collect();
    assert_close(&restored, &doubled, UNIFORM_TOLERANCE);
}

#[test]
fn works_with_gamma_and_negative_min_boost() {
    // A rendition darker than the base in places needs a negative minimum boost.
    let (sdr, hdr) = scene(48, 30, ramp(48, 30));
    let darker: Vec<f32> = hdr.iter().map(|v| v * 0.5).collect();
    let params = GainMapParams {
        min_boost_log2: -1.0,
        max_boost_log2: 2.0,
        hdr_headroom_log2: 2.0,
        gamma: 1.0 / 2.2,
        ..zero_offset_params()
    };
    let restored = round_trip((48, 30), &sdr, &darker, &params, 2.0);
    assert_close(&restored, &darker, RAMP_TOLERANCE);
}

#[test]
fn serialised_metadata_decodes_the_same_map() {
    let (sdr, hdr) = scene(7, 5, |_, _| 1.0);
    let sdr_view = LinearRgbView::new(7, 5, &sdr).unwrap();
    let hdr_view = LinearRgbView::new(7, 5, &hdr).unwrap();
    let (map, metadata) =
        compute_gain_map(&sdr_view, &hdr_view, &GainMapParams::default(), &|| false).unwrap();
    let reread = iso21496::from_bytes(&iso21496::to_bytes(&metadata).unwrap()).unwrap();
    assert_eq!(
        apply_gain_map(&sdr_view, &map, &metadata, 3.0).unwrap(),
        apply_gain_map(&sdr_view, &map, &reread, 3.0).unwrap()
    );
}
