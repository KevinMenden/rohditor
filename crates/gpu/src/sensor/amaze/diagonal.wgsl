// Port of amaze/diagonal.rs: diagonal R/B estimates and a second green pass.

// Diagonal gradients and squared diagonal differences of green. This stage
// walks even columns (`cc in (6..250).step_by(2)`), and the red/blue site of
// the pair is column `cc + green_at_even(r)`.
@compute @workgroup_size(64)
fn diagonal_gradients(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 6);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y;
    if green_at_even(r) == 0 {
        // Red/blue at even columns: green sits at the odd neighbour.
        let g = at(CFA, r, c + 1);
        pput(DELP, r, c, abs(at(CFA, r - 1, c + 1) - at(CFA, r + 1, c - 1)));
        pput(DELM, r, c, abs(at(CFA, r + 1, c + 1) - at(CFA, r - 1, c - 1)));
        pput(SQ1P, r, c, sqr(g - at(CFA, r + 1, c)) + sqr(g - at(CFA, r - 1, c + 2)));
        pput(SQ1M, r, c, sqr(g - at(CFA, r - 1, c)) + sqr(g - at(CFA, r + 1, c + 2)));
    } else {
        let g = at(CFA, r, c);
        pput(SQ1P, r, c, sqr(g - at(CFA, r + 1, c - 1)) + sqr(g - at(CFA, r - 1, c + 1)));
        pput(SQ1M, r, c, sqr(g - at(CFA, r - 1, c - 1)) + sqr(g - at(CFA, r + 1, c + 1)));
        pput(DELP, r, c, abs(at(CFA, r - 1, c + 2) - at(CFA, r + 1, c)));
        pput(DELM, r, c, abs(at(CFA, r + 1, c + 2) - at(CFA, r - 1, c)));
    }
}

// GAUSSEVEN-weighted neighbourhood of a diagonal-difference plane around the
// pair of column `c` in row `r`.
fn even_kernel(plane: u32, r: i32, c: i32) -> f32 {
    return GAUSSEVEN[0] * (pat(plane, r - 1, c) + pat(plane, r, c - 1) + pat(plane, r, c + 1) + pat(plane, r + 1, c))
        + GAUSSEVEN[1] * (pat(plane, r - 2, c - 1) + pat(plane, r - 2, c + 1) + pat(plane, r - 1, c - 2) + pat(plane, r - 1, c + 2)
            + pat(plane, r + 1, c - 2) + pat(plane, r + 1, c + 2) + pat(plane, r + 2, c - 1) + pat(plane, r + 2, c + 1));
}

// Diagonal estimate of the opposite colour from the diagonal neighbour `near`
// and the one beyond it, `far`: adaptive ratio, falling back to Hamilton-Adams
// where the ratio is implausible.
fn diagonal_estimate(centre: f32, near: f32, far: f32) -> f32 {
    let ratio = 2.0 * near / (EPS + centre + far);
    return select(near + 0.5 * (centre - far), centre * ratio, abs(1.0 - ratio) < ARTHRESH);
}

// Bound an estimate in regions of high saturation; `a`, `b` are the two
// neighbours on its diagonal. Also used for the second green pass.
fn bound_estimate(estimate: f32, centre: f32, a: f32, b: f32) -> f32 {
    var value = estimate;
    if value < centre {
        if 2.0 * value < centre {
            value = median(value, a, b);
        } else {
            let weight = 2.0 * (centre - value) / (EPS + value + centre);
            value = weight * value + (1.0 - weight) * median(value, a, b);
        }
    }
    return select(value, median(value, a, b), value > params.clip.x);
}

// Estimate the opposite colour at every red/blue site along both diagonals and
// the relative weight of the two.
@compute @workgroup_size(64)
fn diagonal_estimates(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 8);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    let centre = at(CFA, r, c);
    // Neighbours on the south-east, north-west, north-east and south-west
    // diagonals, one and two steps out.
    let se1 = at(CFA, r + 1, c + 1);
    let nw1 = at(CFA, r - 1, c - 1);
    let ne1 = at(CFA, r - 1, c + 1);
    let sw1 = at(CFA, r + 1, c - 1);
    let rbse = diagonal_estimate(centre, se1, at(CFA, r + 2, c + 2));
    let rbnw = diagonal_estimate(centre, nw1, at(CFA, r - 2, c - 2));
    let rbne = diagonal_estimate(centre, ne1, at(CFA, r - 2, c + 2));
    let rbsw = diagonal_estimate(centre, sw1, at(CFA, r + 2, c - 2));

    let wtse = EPS + pat(DELM, r, c) + pat(DELM, r + 1, c + 1) + pat(DELM, r + 2, c + 2);
    let wtnw = EPS + pat(DELM, r, c) + pat(DELM, r - 1, c - 1) + pat(DELM, r - 2, c - 2);
    let wtne = EPS + pat(DELP, r, c) + pat(DELP, r - 1, c + 1) + pat(DELP, r - 2, c + 2);
    let wtsw = EPS + pat(DELP, r, c) + pat(DELP, r + 1, c - 1) + pat(DELP, r + 2, c - 2);

    let rbm = (wtse * rbnw + wtnw * rbse) / (wtse + wtnw);
    let rbp = (wtne * rbsw + wtsw * rbne) / (wtne + wtsw);

    // Variance of R-B in the plus/minus directions.
    let rbvarm = EPSSQ + even_kernel(SQ1M, r, c);
    pput(PMWT, r, c, rbvarm / ((EPSSQ + even_kernel(SQ1P, r, c)) + rbvarm));

    pput(RBP, r, c, bound_estimate(rbp, centre, sw1, ne1));
    pput(RBM, r, c, bound_estimate(rbm, centre, nw1, se1));
}

// Let the plus/minus weight follow its diagonal neighbours where they
// discriminate better, and form R+B at each red/blue site. Ordered kernel with
// the same row-to-row dependency as `populate_green`, here on `pmwt`.
@compute @workgroup_size(128)
fn combine_diagonals(@builtin(local_invocation_id) local: vec3<u32>, @builtin(workgroup_id) group: vec3<u32>) {
    base = group.z * TILE_WORDS;
    let q = i32(local.x);
    // Row 9 is never updated: its original weights are the first "previous row".
    chain[128u + local.x] = pat(PMWT, 9, 2 * q);
    workgroupBarrier();
    for (var r = 10; r < TSI - 10; r++) {
        let c = 2 * q + green_at_even(r);
        var weight = pat(PMWT, r, c);
        if q >= 5 && q < (TSI - 10) / 2 {
            let previous = u32((r - 1) & 1) * 128u;
            let alt = (chain[previous + u32((c - 1) >> 1u)] + chain[previous + u32((c + 1) >> 1u)]
                + pat(PMWT, r + 1, c - 1) + pat(PMWT, r + 1, c + 1)) / 4.0;
            if abs(0.5 - weight) < abs(0.5 - alt) { weight = alt; }
            pput(PMWT, r, c, weight);
            // This is R+B, interpolated.
            pput(RBINT, r, c, 0.5 * (at(CFA, r, c) + pat(RBM, r, c) * (1.0 - weight) + pat(RBP, r, c) * weight));
        }
        chain[u32(r & 1) * 128u + local.x] = weight;
        workgroupBarrier();
    }
}

// Green estimate from `sample`, the cardinal neighbour of a red/blue site whose
// R+B is `centre`, and `other`, the R+B of the next such site in that
// direction: adaptive ratio, falling back to Hamilton-Adams.
fn green_from_rb(sample_value: f32, centre: f32, other: f32) -> f32 {
    let ratio = sample_value * 2.0 / (EPS + centre + other);
    return select(sample_value + 0.5 * (centre - other), centre * ratio, abs(1.0 - ratio) < ARTHRESH);
}

// Second green pass at red/blue sites using R+B. Skipped where the diagonal
// weight is more decisive than the vertical/horizontal weight.
@compute @workgroup_size(64)
fn refine_green_with_rb(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 12);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    let hvwt = pat(HVWT, r, c);
    if abs(0.5 - pat(PMWT, r, c)) < abs(0.5 - hvwt) { return; }

    // Interpolate G vertically/horizontally using R+B values. R+B exists only
    // at red/blue sites, so the vertical neighbour's value comes from two rows
    // away.
    let centre = pat(RBINT, r, c);
    let up = at(CFA, r - 1, c);
    let down = at(CFA, r + 1, c);
    let left = at(CFA, r, c - 1);
    let right = at(CFA, r, c + 1);
    let gu = green_from_rb(up, centre, pat(RBINT, r - 2, c));
    let gd = green_from_rb(down, centre, pat(RBINT, r + 2, c));
    let gl = green_from_rb(left, centre, pat(RBINT, r, c - 2));
    let gr = green_from_rb(right, centre, pat(RBINT, r, c + 2));

    // Interpolated G via adaptive weights of the cardinal evaluations.
    let gintv = (at(W0, r - 1, c) * gd + at(W0, r + 1, c) * gu) / (at(W0, r + 1, c) + at(W0, r - 1, c));
    let ginth = (at(W1, r, c - 1) * gr + at(W1, r, c + 1) * gl) / (at(W1, r, c - 1) + at(W1, r, c + 1));

    let bounded_v = bound_estimate(gintv, centre, up, down);
    let bounded_h = bound_estimate(ginth, centre, left, right);
    let g = bounded_h * (1.0 - hvwt) + bounded_v * hvwt;
    put(GREEN, r, c, g);
    pput(DGRB0, r, c, g - at(CFA, r, c));
}
