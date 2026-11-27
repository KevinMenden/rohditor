//! RGB histogram drawn as translucent filled curves.

use gpui::{Bounds, Hsla, IntoElement, PathBuilder, Pixels, canvas, point, prelude::*, px, rgba};
use rohditor_core::Histogram;

/// Height of every bin relative to the highest. The outermost bins collect clipped pixels and
/// would flatten the rest, so they do not set the scale.
fn heights(bins: &[u64], peak: u64) -> Vec<f32> {
    bins.iter()
        .map(|&count| (count as f32 / peak.max(1) as f32).sqrt().min(1.0))
        .collect()
}

fn peak(histogram: &Histogram) -> u64 {
    [&histogram.red, &histogram.green, &histogram.blue]
        .into_iter()
        .flat_map(|bins| &bins[1..bins.len() - 1])
        .copied()
        .max()
        .unwrap_or(0)
}

pub fn histogram(histogram: Option<&Histogram>) -> impl IntoElement + use<> {
    channels(histogram).w_full().h(px(84.0))
}

/// The histogram filling its parent, behind what else it holds.
pub fn histogram_backdrop(histogram: Option<&Histogram>) -> impl IntoElement + use<> {
    channels(histogram).absolute().size_full()
}

fn channels(histogram: Option<&Histogram>) -> gpui::Canvas<()> {
    let channels: Vec<(Vec<f32>, Hsla)> = histogram
        .map(|histogram| {
            let peak = peak(histogram);
            vec![
                (heights(&histogram.red, peak), rgba(0xd9606670).into()),
                (heights(&histogram.green, peak), rgba(0x6fbf7370).into()),
                (heights(&histogram.blue, peak), rgba(0x5f93e070).into()),
            ]
        })
        .unwrap_or_default();
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            for (heights, color) in &channels {
                if let Some(path) = curve(bounds, heights) {
                    window.paint_path(path, *color);
                }
            }
        },
    )
}

fn curve(bounds: Bounds<Pixels>, heights: &[f32]) -> Option<gpui::Path<Pixels>> {
    let mut path = PathBuilder::fill();
    path.move_to(bounds.bottom_left());
    let step = bounds.size.width / (heights.len().max(2) - 1) as f32;
    for (index, height) in heights.iter().enumerate() {
        path.line_to(point(
            bounds.left() + step * index as f32,
            bounds.bottom() - bounds.size.height * *height,
        ));
    }
    path.line_to(bounds.bottom_right());
    path.close();
    path.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipped_bins_do_not_set_the_scale() {
        let mut histogram = Histogram::default();
        histogram.red[0] = 1_000_000;
        histogram.green[100] = 400;
        histogram.blue[101] = 100;
        let peak = peak(&histogram);
        assert_eq!(peak, 400);
        assert_eq!(heights(&histogram.red, peak)[0], 1.0);
        assert_eq!(heights(&histogram.blue, peak)[101], 0.5);
    }
}
