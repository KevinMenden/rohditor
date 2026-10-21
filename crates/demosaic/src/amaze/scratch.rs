//! Per-tile working planes.
//!
//! RawTherapee shares buffers between stages whose lifetimes do not overlap;
//! here every plane has its own storage, trading about 5 MB per scratch set
//! for stages that can be read in isolation. Planes marked "half" hold one
//! value per horizontal pixel pair and are addressed with `index >> 1`.

use rohditor_image::allocate_zeroed_f32;

use super::math::{TS, TSH};
use crate::DemosaicError;

const FULL: usize = TS * TS;
const HALF: usize = TS * TSH;

pub(super) struct AmazeScratch {
    /// Tile CFA samples.
    pub(super) cfa: Vec<f32>,
    /// Green: the CFA copy, replaced by interpolated values at red/blue sites.
    pub(super) green: Vec<f32>,
    /// Sum of squared horizontal and vertical gradients.
    pub(super) delhvsqsum: Vec<f32>,
    /// Vertical / horizontal gradient-based direction weights.
    pub(super) dirwts0: Vec<f32>,
    pub(super) dirwts1: Vec<f32>,
    /// Vertically / horizontally interpolated colour differences (G - R, G - B).
    pub(super) vcd: Vec<f32>,
    pub(super) hcd: Vec<f32>,
    /// Alternative (Hamilton-Adams) vertical / horizontal colour differences.
    pub(super) vcdalt: Vec<f32>,
    pub(super) hcdalt: Vec<f32>,
    /// Square of the vertical-minus-horizontal colour difference.
    pub(super) cddiffsq: Vec<f32>,
    /// Differences of the up/down and left/right green interpolations.
    pub(super) dgintv: Vec<f32>,
    pub(super) dginth: Vec<f32>,
    /// Horizontal-versus-vertical weight (half).
    pub(super) hvwt: Vec<f32>,
    /// Final colour differences G - R (`[0]`) and G - B (`[1]`) (half).
    pub(super) dgrb: [Vec<f32>; 2],
    /// Horizontal and vertical curvature of the interpolated green (half).
    pub(super) dgrb2_h: Vec<f32>,
    pub(super) dgrb2_v: Vec<f32>,
    /// Gradients in the plus (NE/SW) and minus (NW/SE) directions (half).
    pub(super) delp: Vec<f32>,
    pub(super) delm: Vec<f32>,
    /// Squared diagonal colour differences (half).
    pub(super) dgrbsq1m: Vec<f32>,
    pub(super) dgrbsq1p: Vec<f32>,
    /// Nyquist test statistic (half).
    pub(super) nyqutest: Vec<f32>,
    /// Weight combining the plus and minus diagonal estimates (half).
    pub(super) pmwt: Vec<f32>,
    /// Interpolated R-B in the minus and plus directions (half).
    pub(super) rbm: Vec<f32>,
    pub(super) rbp: Vec<f32>,
    /// Diagonally interpolated R+B (half).
    pub(super) rbint: Vec<f32>,
    /// Nyquist texture flags before and after neighbourhood voting (half).
    pub(super) nyquist: Vec<u8>,
    pub(super) nyquist2: Vec<u8>,
    /// Reconstructed red and blue for the tile's valid interior.
    pub(super) red: Vec<f32>,
    pub(super) blue: Vec<f32>,
}

impl AmazeScratch {
    pub(super) fn new() -> Result<Self, DemosaicError> {
        let full = || allocate_zeroed_f32(FULL).map_err(DemosaicError::from);
        let half = || allocate_zeroed_f32(HALF).map_err(DemosaicError::from);
        Ok(Self {
            cfa: full()?,
            green: full()?,
            delhvsqsum: full()?,
            dirwts0: full()?,
            dirwts1: full()?,
            vcd: full()?,
            hcd: full()?,
            vcdalt: full()?,
            hcdalt: full()?,
            cddiffsq: full()?,
            dgintv: full()?,
            dginth: full()?,
            hvwt: half()?,
            dgrb: [half()?, half()?],
            dgrb2_h: half()?,
            dgrb2_v: half()?,
            delp: half()?,
            delm: half()?,
            dgrbsq1m: half()?,
            dgrbsq1p: half()?,
            nyqutest: half()?,
            pmwt: half()?,
            rbm: half()?,
            rbp: half()?,
            rbint: half()?,
            nyquist: vec![0; HALF],
            nyquist2: vec![0; HALF],
            red: full()?,
            blue: full()?,
        })
    }

    /// Reset what the stages expect to find zeroed. Upstream gets this from
    /// `calloc` and `memset`.
    ///
    /// Every other plane is fully written before it is read, except `vcd`,
    /// `hcd`, `vcdalt` and `hcdalt`: the variance stage reads them up to two
    /// rows and columns outside the region the stages write (the outermost
    /// rows and columns of the tile), and upstream relies on those entries
    /// being zero from `calloc`. Nothing writes them here, so they stay zero.
    /// Debug builds poison the remaining planes with NaN so a stage that reads
    /// an unwritten entry fails the finite-output check instead of silently
    /// depending on the previous tile.
    pub(super) fn clear(&mut self) {
        self.nyquist.fill(0);
        self.nyquist2.fill(0);
        self.dgrb2_h.fill(0.0);
        self.dgrb2_v.fill(0.0);
        #[cfg(debug_assertions)]
        self.poison();
    }

    #[cfg(debug_assertions)]
    fn poison(&mut self) {
        let [dgrb_red, dgrb_blue] = &mut self.dgrb;
        for plane in [
            dgrb_red,
            dgrb_blue,
            &mut self.delhvsqsum,
            &mut self.dirwts0,
            &mut self.dirwts1,
            &mut self.cddiffsq,
            &mut self.dgintv,
            &mut self.dginth,
            &mut self.hvwt,
            &mut self.delp,
            &mut self.delm,
            &mut self.dgrbsq1m,
            &mut self.dgrbsq1p,
            &mut self.nyqutest,
            &mut self.pmwt,
            &mut self.rbm,
            &mut self.rbp,
            &mut self.rbint,
            &mut self.red,
            &mut self.blue,
        ] {
            plane.fill(f32::NAN);
        }
    }
}
