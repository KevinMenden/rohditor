//! Clipped-region segmentation on the Bayer cell grid.
//!
//! Every cell that holds a suspected-clipped sample is "masked". The mask is
//! dilated by [`DILATION_CELLS`] so each region also owns a ring of clean
//! border cells, then connected components (8-connectivity) of the dilated
//! mask are labelled with a deterministic two-pass union-find. Labels are
//! compact (`1..=count`, `0` = outside every region) and independent of
//! thread count.

use rayon::prelude::*;

use crate::cells::{CellSummaries, allocation_error, ceil_halved};
use crate::{CancellationCheck, HighlightError, checkpoint};

/// How far (in cells, Chebyshev) the clipped mask grows to reach clean
/// border cells. One cell is two photosites, so this is a 2 px band.
pub(crate) const DILATION_CELLS: usize = 1;

/// Rows between cancellation checks in the sequential labelling passes.
const CHECKPOINT_ROWS: usize = 64;

/// Per-cell region labels plus the channels clipped inside each region.
#[derive(Debug)]
pub(crate) struct Regions {
    width: usize,
    labels: Vec<u32>,
    count: u32,
    /// Index = label (entry 0 unused). Bit `c` = region has a suspected
    /// clipped sample in channel `c`.
    clipped_channels: Vec<u8>,
}

impl Regions {
    pub(crate) fn build(
        summaries: &CellSummaries,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Self, HighlightError> {
        let (width, height) = (summaries.width, summaries.height);
        let mut labels = dilated_mask(summaries, cancellation)?;
        let count = label_components(&mut labels, width, height, cancellation)?;

        let mut clipped_channels = Vec::new();
        clipped_channels
            .try_reserve_exact(count as usize + 1)
            .map_err(|_| allocation_error(count as usize + 1))?;
        clipped_channels.resize(count as usize + 1, 0_u8);
        for y in 0..height {
            if y % CHECKPOINT_ROWS == 0 {
                checkpoint(cancellation)?;
            }
            for x in 0..width {
                let label = labels[y * width + x];
                if label != 0 {
                    clipped_channels[label as usize] |= summaries.clipped_channels_at(x, y);
                }
            }
        }
        Ok(Self {
            width,
            labels,
            count,
            clipped_channels,
        })
    }

    /// Number of regions; labels run `1..=count`.
    pub(crate) const fn count(&self) -> u32 {
        self.count
    }

    /// Region label of a cell (`0` = outside every region).
    pub(crate) fn label(&self, x: usize, y: usize) -> u32 {
        self.labels[y * self.width + x]
    }

    /// Labels of one cell row.
    pub(crate) fn row(&self, y: usize) -> &[u32] {
        &self.labels[y * self.width..(y + 1) * self.width]
    }

    /// Channels with suspected-clipped samples inside `label`.
    pub(crate) fn clipped_channels(&self, label: u32) -> u8 {
        self.clipped_channels[label as usize]
    }
}

/// Logical upper bound of the segmentation's working memory.
///
/// - labels: 4 B per cell;
/// - dilation scratch (freed before labelling): 1 B per cell;
/// - union-find parents: a new provisional label needs a neighbourhood with
///   no labelled cell, so new labels are >= 2 cells apart, giving at most
///   `ceil(w/2) * ceil(h/2) + 1` entries of 4 B;
/// - per-region channel masks: two regions have raw-mask cells at least
///   `2 * DILATION_CELLS + 2` apart, so there are at most
///   `ceil(w/s) * ceil(h/s)` of them for that spacing `s` (see
///   [`max_regions`]), plus the unused entry 0, 1 B each.
pub(crate) fn scratch_bytes(cell_width: usize, cell_height: usize) -> Option<usize> {
    let cells = cell_width.checked_mul(cell_height)?;
    let labels = cells.checked_mul(std::mem::size_of::<u32>())?;
    let dilation = cells;
    let provisional = ceil_halved(cell_width)
        .checked_mul(ceil_halved(cell_height))?
        .checked_add(1)?
        .checked_mul(std::mem::size_of::<u32>())?;
    let regions = max_regions(cell_width, cell_height)?;
    labels
        .checked_add(dilation)?
        .checked_add(provisional)?
        .checked_add(regions)
}

/// Upper bound on the number of regions on a `cell_width x cell_height` grid
/// (plus one for the unused label 0).
pub(crate) fn max_regions(cell_width: usize, cell_height: usize) -> Option<usize> {
    let spacing = 2 * DILATION_CELLS + 2;
    cell_width
        .div_ceil(spacing)
        .checked_mul(cell_height.div_ceil(spacing))?
        .checked_add(1)
}

/// Dilate the clipped-cell mask separably. Returns `1` for cells inside the
/// dilated mask and `0` elsewhere, one `u32` per cell, so the buffer can be
/// labelled in place.
fn dilated_mask(
    summaries: &CellSummaries,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<u32>, HighlightError> {
    let (width, height) = (summaries.width, summaries.height);
    let cells = width
        .checked_mul(height)
        .ok_or_else(|| allocation_error(usize::MAX))?;

    // Horizontal pass: distance to the nearest masked cell in the same row.
    let mut horizontal: Vec<u8> = Vec::new();
    horizontal
        .try_reserve_exact(cells)
        .map_err(|_| allocation_error(cells))?;
    horizontal.resize(cells, 0);
    horizontal.par_chunks_mut(width).enumerate().try_for_each(
        |(y, row)| -> Result<(), HighlightError> {
            checkpoint(cancellation)?;
            let mut last_masked: Option<usize> = None;
            for (x, flag) in row.iter_mut().enumerate() {
                if summaries.clipped_channels_at(x, y) != 0 {
                    last_masked = Some(x);
                }
                *flag = u8::from(last_masked.is_some_and(|m| x - m <= DILATION_CELLS));
            }
            let mut next_masked: Option<usize> = None;
            for (x, flag) in row.iter_mut().enumerate().rev() {
                if summaries.clipped_channels_at(x, y) != 0 {
                    next_masked = Some(x);
                }
                *flag |= u8::from(next_masked.is_some_and(|m| m - x <= DILATION_CELLS));
            }
            Ok(())
        },
    )?;

    // Vertical pass: OR the rows within the dilation radius.
    let mut labels: Vec<u32> = Vec::new();
    labels
        .try_reserve_exact(cells)
        .map_err(|_| allocation_error(cells))?;
    labels.resize(cells, 0);
    labels.par_chunks_mut(width).enumerate().try_for_each(
        |(y, out)| -> Result<(), HighlightError> {
            checkpoint(cancellation)?;
            let first = y.saturating_sub(DILATION_CELLS);
            let last = (y + DILATION_CELLS).min(height - 1);
            for source_y in first..=last {
                let source = &horizontal[source_y * width..(source_y + 1) * width];
                for (flag, &masked) in out.iter_mut().zip(source) {
                    *flag |= u32::from(masked);
                }
            }
            Ok(())
        },
    )?;
    Ok(labels)
}

/// Two-pass 8-connected labelling of the non-zero cells of `labels`.
/// Returns the number of components; afterwards every non-zero cell holds a
/// compact label in `1..=count`, numbered by first appearance in raster order.
fn label_components(
    labels: &mut [u32],
    width: usize,
    height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<u32, HighlightError> {
    // Union-find parents; entry 0 is the background. Roots are always the
    // smallest member of their set, so `parent[i] <= i` holds throughout.
    let capacity = ceil_halved(width)
        .checked_mul(ceil_halved(height))
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| allocation_error(usize::MAX))?;
    let mut parent: Vec<u32> = Vec::new();
    parent
        .try_reserve_exact(capacity)
        .map_err(|_| allocation_error(capacity))?;
    parent.push(0);

    for y in 0..height {
        if y % CHECKPOINT_ROWS == 0 {
            checkpoint(cancellation)?;
        }
        for x in 0..width {
            let index = y * width + x;
            if labels[index] == 0 {
                continue;
            }
            // Already-visited neighbours: W, NW, N, NE.
            let mut neighbours = [0_u32; 4];
            if x > 0 {
                neighbours[0] = labels[index - 1];
            }
            if y > 0 {
                if x > 0 {
                    neighbours[1] = labels[index - width - 1];
                }
                neighbours[2] = labels[index - width];
                if x + 1 < width {
                    neighbours[3] = labels[index - width + 1];
                }
            }
            let mut smallest_root = 0_u32;
            for &label in neighbours.iter().filter(|&&label| label != 0) {
                let root = find_root(&mut parent, label);
                if smallest_root == 0 || root < smallest_root {
                    smallest_root = root;
                }
            }
            if smallest_root == 0 {
                let fresh = u32::try_from(parent.len()).map_err(|_| too_many_regions())?;
                parent.push(fresh);
                labels[index] = fresh;
            } else {
                for &label in neighbours.iter().filter(|&&label| label != 0) {
                    let root = find_root(&mut parent, label);
                    parent[root as usize] = smallest_root;
                }
                labels[index] = smallest_root;
            }
        }
    }
    checkpoint(cancellation)?;

    // Compact roots to 1..=count in place: a root takes the next id, any
    // other entry copies the (already finalised) id of its smaller parent.
    let mut count = 0_u32;
    for provisional in 1..parent.len() {
        if parent[provisional] as usize == provisional {
            count += 1;
            parent[provisional] = count;
        } else {
            parent[provisional] = parent[parent[provisional] as usize];
        }
    }
    for (row, chunk) in labels.chunks_mut(width).enumerate() {
        if row % CHECKPOINT_ROWS == 0 {
            checkpoint(cancellation)?;
        }
        for label in chunk.iter_mut().filter(|label| **label != 0) {
            *label = parent[*label as usize];
        }
    }
    Ok(count)
}

/// Root of `label`'s set with path halving.
fn find_root(parent: &mut [u32], mut label: u32) -> u32 {
    while parent[label as usize] != label {
        let grandparent = parent[parent[label as usize] as usize];
        parent[label as usize] = grandparent;
        label = grandparent;
    }
    label
}

fn too_many_regions() -> HighlightError {
    HighlightError::Image(rohditor_image::ImageError::InvalidDimensions {
        width: 0,
        height: 0,
        row_stride: 0,
        reason: "clipped-region label count overflowed u32".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(grid: &[&str]) -> (Vec<u32>, u32) {
        let (width, height) = (grid[0].len(), grid.len());
        let mut labels: Vec<u32> = grid
            .iter()
            .flat_map(|row| row.bytes().map(|cell| u32::from(cell == b'#')))
            .collect();
        let count =
            label_components(&mut labels, width, height, &|| false).expect("labelling succeeds");
        (labels, count)
    }

    #[test]
    fn empty_grid_has_no_regions() {
        let (labels, count) = label(&["....", "...."]);
        assert_eq!(count, 0);
        assert!(labels.iter().all(|&label| label == 0));
    }

    #[test]
    fn separate_blobs_get_compact_labels_in_raster_order() {
        let (labels, count) = label(&[
            "##...#", //
            "##...#", //
            "......", //
            "..##..", //
        ]);
        assert_eq!(count, 3);
        assert_eq!(
            labels,
            [
                1, 1, 0, 0, 0, 2, //
                1, 1, 0, 0, 0, 2, //
                0, 0, 0, 0, 0, 0, //
                0, 0, 3, 3, 0, 0,
            ]
        );
    }

    #[test]
    fn diagonal_neighbours_are_connected() {
        let (labels, count) = label(&["#...", ".#..", "..#."]);
        assert_eq!(count, 1);
        assert_eq!(labels.iter().filter(|&&label| label == 1).count(), 3);
    }

    #[test]
    fn u_shape_merges_arms_labelled_apart_in_the_first_pass() {
        // The two arms start as different provisional labels and only the
        // bottom row joins them; the second pass must unify and compact.
        let (labels, count) = label(&[
            "#...#", //
            "#...#", //
            "#####", //
            ".....", //
            "..#..",
        ]);
        assert_eq!(count, 2);
        assert_eq!(labels[0], 1);
        assert_eq!(labels[4], 1);
        assert_eq!(labels[2 * 5 + 2], 1);
        assert_eq!(labels[4 * 5 + 2], 2);
    }

    #[test]
    fn labelling_observes_cancellation() {
        let mut labels = vec![1, 0, 1, 0];
        assert_eq!(
            label_components(&mut labels, 2, 2, &|| true),
            Err(HighlightError::Cancelled)
        );
    }

    #[test]
    fn region_count_stays_within_the_scratch_bound_for_the_densest_layout() {
        // Raw-mask cells spaced exactly `2 * DILATION_CELLS + 2` apart are
        // the densest layout of separate regions.
        let spacing = 2 * DILATION_CELLS + 2;
        let (width, height) = (5 * spacing, 3 * spacing);
        let mut labels = vec![0_u32; width * height];
        for y in (0..height).step_by(spacing) {
            for x in (0..width).step_by(spacing) {
                for dy in 0..=2 * DILATION_CELLS {
                    for dx in 0..=2 * DILATION_CELLS {
                        if y + dy < height && x + dx < width {
                            labels[(y + dy) * width + x + dx] = 1;
                        }
                    }
                }
            }
        }
        let count = label_components(&mut labels, width, height, &|| false).expect("labels");
        assert_eq!(count as usize, 15);
        assert!((count as usize) < max_regions(width, height).expect("bound"));
    }
}
