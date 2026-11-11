//! The seven named ground-truth scenes of the issue #63 harness.
//!
//! Every scene has odd dimensions (the Bayer phase comes from
//! `(x & 1, y & 1)`, so an odd row stride shifts the phase at every row
//! start — stride and width/height mix-ups cannot hide), off-center
//! asymmetric geometry, and backgrounds whose per-channel maxima outside
//! the objects stay <= 0.40. Design rules (checked by `scene_guards` at
//! BOTH sat levels): channels that clip have scaled peaks >= 1.2; channels
//! that must not clip peak <= 0.90 globally.

use super::generator::{Object, Profile, SceneSpec, Shape};

// Shared background gradients: `b0` in (0.18, 0.30) at (0, 0), slopes
// |b1|, |b2| <= 0.14, per-channel maxima (0.39, 0.39, 0.35) <= 0.40.
// The slope signs differ per channel so no two channels share a symmetry
// axis (asymmetry rule).
const BG_R: [f32; 3] = [0.22, 0.12, 0.05];
const BG_G: [f32; 3] = [0.26, -0.08, 0.13];
const BG_B: [f32; 3] = [0.24, 0.11, -0.09];
const BG: [[f32; 3]; 3] = [BG_R, BG_G, BG_B];
const NO_WB: [f32; 3] = [1.0, 1.0, 1.0];

/// Small ~9 px G disc: edge/fallback-dominated, tiny enough that per-site
/// differences can be eyeballed (issue's "around 8 px" scene).
pub const DISK_SMALL: SceneSpec = SceneSpec {
    name: "disk_small",
    width: 19,
    height: 17,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[Object {
        // Sits on a G phase at (9, 8): phase ((8&1)<<1)|(9&1) = 1 = Green.
        shape: Shape::Disc {
            cx: 9.0,
            cy: 8.0,
            r: 4.5,
            profile: Profile::Smooth,
        },
        peak: [0.30, 1.70, 0.18],
    }],
    clips: [false, true, false],
};

/// The #62 repro scene, sized to the issue's radius reading: a ~148 px G
/// disc in a 181x177 frame reproduces the issue's ~8k suspected sites at
/// sat 1.0 (G holds half of the RGGB grid, so a nearly fully blown disc of
/// radius ~72 px measures pi*r^2/2 ~ 8.1k sites). The flat-top profile
/// keeps a super-saturated core with a near-but-below-sat edge falloff.
/// Single-channel regions are where estimates stop being applied today:
/// `recon` (estimates produced) far exceeds `raised` (sites actually
/// lifted at the clipping edge).
pub const DISK_LARGE: SceneSpec = SceneSpec {
    name: "disk_large",
    width: 181,
    height: 177,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[Object {
        // Center phase ((82&1)<<1)|(86&1) = 0 = Red; the disc is wide
        // enough that the suspected set is dominated by interior G sites
        // anyway. Peak 6.0 blows nearly the whole disc so the suspected
        // G count reaches the issue's ~8k scale; at that steepness the
        // below-sat edge ring is narrow and stays clear of the margin
        // guard without jitter.
        shape: Shape::Disc {
            cx: 86.0,
            cy: 82.0,
            r: 74.0,
            profile: Profile::FlatTop,
        },
        peak: [0.30, 6.00, 0.18],
    }],
    clips: [false, true, false],
};

/// The regime region-aware Opposed (#66) targets: a large G highlight with a
/// wide soft shoulder on a nearly neutral object (R and B rise at about 60% of
/// the rate of G), so the chromatic offset at the clip boundary describes the
/// whole blown interior. The clipped region is ~50 px across, far beyond the
/// ~12 px that local candidate search can reach, while the shoulder keeps
/// plenty of near-clip border cells around it. `DISK_LARGE` cannot serve
/// this purpose: its truth G is 6.0 against R/B of ~0.5 with a ~2 px edge,
/// so no chroma model can recover it.
pub const DISK_SOFT: SceneSpec = SceneSpec {
    name: "disk_soft",
    width: 151,
    height: 141,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[Object {
        shape: Shape::Disc {
            cx: 70.0,
            cy: 66.0,
            r: 70.0,
            profile: Profile::Smooth,
        },
        peak: [0.58, 0.9519, 0.58],
    }],
    clips: [false, true, false],
};

/// Opposed-channel case: R and G clip, B stays clean to oppose with.
/// rx != ry and a lower-right-of-center placement break symmetry.
pub const TWO_CHANNEL: SceneSpec = SceneSpec {
    name: "two_channel",
    width: 41,
    height: 37,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[Object {
        // Sits on a G phase at (24, 21): ((21&1)<<1)|(24&1) = 2 -> Green.
        shape: Shape::Ellipse {
            cx: 24.0,
            cy: 21.0,
            rx: 9.0,
            ry: 6.5,
        },
        peak: [1.55, 1.45, 0.62],
    }],
    clips: [true, true, false],
};

/// Lamp/sun semantics: a fully blown core (no opposing support -> honest
/// fallback ring) inside a partially clipping halo. The non-identity WB
/// scale makes R clip farthest and B intermediate -> channel-asymmetric
/// clip radii.
pub const SUN_CORE_HALO: SceneSpec = SceneSpec {
    name: "sun_core_halo",
    width: 57,
    height: 53,
    bg: BG,
    wb_scale: [1.9, 1.0, 1.5],
    objects: &[
        Object {
            shape: Shape::Disc {
                cx: 26.0,
                cy: 24.0,
                r: 7.5,
                profile: Profile::FlatTop,
            },
            peak: [1.15, 1.20, 1.10],
        },
        Object {
            // Same center, additive inside the core field.
            shape: Shape::Disc {
                cx: 26.0,
                cy: 24.0,
                r: 19.0,
                profile: Profile::Smooth,
            },
            // Jittered 2026-11-04 from 0.65 -> 0.6437: the halo's (1 - t²)²
            // falloff landed B truth on 0.9800638 at (32, 17), tripping
            // the 1e-4 margin guard at sat = 0.98.
            peak: [0.75, 0.80, 0.6437],
        },
    ],
    clips: [true, true, true],
};

/// Two clipped objects of different color, adjacent but non-overlapping
/// (center distance ~17.1 > 8.5 + 7.5). The clean channels differ per
/// object (warm is B-poor, cool is R-poor) so cross-talk is measurable.
pub const WARM_COOL_ADJACENT: SceneSpec = SceneSpec {
    name: "warm_cool_adjacent",
    width: 49,
    height: 45,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[
        Object {
            shape: Shape::Disc {
                cx: 18.0,
                cy: 25.0,
                r: 8.5,
                profile: Profile::Smooth,
            },
            peak: [1.60, 0.55, 0.35],
        },
        Object {
            shape: Shape::Disc {
                cx: 34.0,
                cy: 19.0,
                r: 7.5,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.50],
        },
    ],
    clips: [true, false, true],
};

/// Clipped plateau flush to the top-left corner: no support beyond the
/// image edge -> exercises `fully_unsupported_sites` / fallback behaviour.
/// The background slopes provide the plateau tilt (the shared BG_G
/// gradient slopes G by -0.08*u); the interior plateau reaches ~1.75 >= 1.2,
/// well clear of the sat margin.
pub const BORDER_CLIP: SceneSpec = SceneSpec {
    name: "border_clip",
    width: 43,
    height: 39,
    bg: BG,
    wb_scale: NO_WB,
    objects: &[Object {
        shape: Shape::CornerRect {
            x_end: 16.0,
            y_end: 22.0,
            feather: 3.0,
        },
        peak: [0.55, 1.50, 0.40],
    }],
    clips: [false, true, false],
};

/// Many tiny speculars (mostly 1-3 site objects): isolated clipped sites
/// with no same-channel support — the estimator produces values there, but
/// none get applied. Each object center is
/// placed on the Bayer phase of its clipping channel; no two objects
/// overlap; exactly two sit within 3 px of the image border
/// ((34, 12) right edge, (29, 27) bottom edge). The non-identity WB scale
/// adds a third clip pattern.
pub const SPECULARS: SceneSpec = SceneSpec {
    name: "speculars",
    width: 37,
    height: 31,
    bg: BG,
    wb_scale: [1.3, 1.0, 1.15],
    objects: &[
        // Warm (clips R; centers on even/even R phases).
        Object {
            shape: Shape::Disc {
                cx: 4.0,
                cy: 6.0,
                r: 2.2,
                profile: Profile::Smooth,
            },
            peak: [1.70, 0.40, 0.30],
        },
        Object {
            shape: Shape::Disc {
                cx: 12.0,
                cy: 4.0,
                r: 0.8,
                profile: Profile::Smooth,
            },
            peak: [1.70, 0.40, 0.30],
        },
        Object {
            shape: Shape::Disc {
                cx: 18.0,
                cy: 4.0,
                r: 1.4,
                profile: Profile::Smooth,
            },
            peak: [1.70, 0.40, 0.30],
        },
        Object {
            shape: Shape::Disc {
                cx: 22.0,
                cy: 26.0,
                r: 1.4,
                profile: Profile::Smooth,
            },
            peak: [1.70, 0.40, 0.30],
        },
        Object {
            shape: Shape::Disc {
                cx: 34.0,
                cy: 12.0,
                r: 2.2,
                profile: Profile::Smooth,
            },
            peak: [1.70, 0.40, 0.30],
        },
        // Cool (clips B; centers on odd/odd B phases).
        Object {
            shape: Shape::Disc {
                cx: 7.0,
                cy: 17.0,
                r: 1.4,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.60],
        },
        Object {
            shape: Shape::Disc {
                cx: 15.0,
                cy: 11.0,
                r: 2.2,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.60],
        },
        Object {
            shape: Shape::Disc {
                cx: 25.0,
                cy: 5.0,
                r: 0.8,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.60],
        },
        Object {
            shape: Shape::Disc {
                cx: 5.0,
                cy: 25.0,
                r: 0.8,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.60],
        },
        Object {
            shape: Shape::Disc {
                cx: 29.0,
                cy: 27.0,
                r: 1.4,
                profile: Profile::Smooth,
            },
            peak: [0.35, 0.45, 1.60],
        },
        // Green (clips G; centers on mixed-parity G phases).
        Object {
            shape: Shape::Disc {
                cx: 14.0,
                cy: 21.0,
                r: 2.2,
                profile: Profile::Smooth,
            },
            peak: [0.40, 1.70, 0.35],
        },
        Object {
            shape: Shape::Disc {
                cx: 21.0,
                cy: 16.0,
                r: 0.8,
                profile: Profile::Smooth,
            },
            peak: [0.40, 1.70, 0.35],
        },
        Object {
            shape: Shape::Disc {
                cx: 26.0,
                cy: 19.0,
                r: 1.4,
                profile: Profile::Smooth,
            },
            peak: [0.40, 1.70, 0.35],
        },
        Object {
            shape: Shape::Disc {
                cx: 31.0,
                cy: 8.0,
                r: 0.8,
                profile: Profile::Smooth,
            },
            peak: [0.40, 1.70, 0.35],
        },
    ],
    clips: [true, true, true],
};

/// Iteration order for the matrix and guard tests.
pub const ALL: [SceneSpec; 8] = [
    DISK_SMALL,
    DISK_LARGE,
    DISK_SOFT,
    TWO_CHANNEL,
    SUN_CORE_HALO,
    WARM_COOL_ADJACENT,
    BORDER_CLIP,
    SPECULARS,
];

/// Saturation levels swept per the issue ("configurable saturation").
pub const SAT_LEVELS: [f32; 2] = [1.0, 0.98];
