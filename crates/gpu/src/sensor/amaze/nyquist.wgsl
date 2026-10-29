// Port of amaze/nyquist.rs: Nyquist texture detection, area interpolation of
// the direction weight, the final green at red/blue sites, and the curvature
// based refinement of green inside Nyquist areas.

// Compare the smoothed colour-difference energy with the smoothed CFA gradient
// energy at each red/blue site. Where the former wins, the site is in a Nyquist
// texture region; the tile's bounding box of such sites is reduced with atomics
// (the CPU's first/last flagged row and min/max flagged column).
@compute @workgroup_size(64)
fn nyquist_detect(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 6);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    let test = (GAUSSODD[0] * at(CDDIFFSQ, r, c)
        + GAUSSODD[1] * (at(CDDIFFSQ, r - 1, c - 1) + at(CDDIFFSQ, r - 1, c + 1) + at(CDDIFFSQ, r + 1, c - 1) + at(CDDIFFSQ, r + 1, c + 1))
        + GAUSSODD[2] * (at(CDDIFFSQ, r - 2, c) + at(CDDIFFSQ, r, c - 2) + at(CDDIFFSQ, r, c + 2) + at(CDDIFFSQ, r + 2, c))
        + GAUSSODD[3] * (at(CDDIFFSQ, r - 2, c - 2) + at(CDDIFFSQ, r - 2, c + 2) + at(CDDIFFSQ, r + 2, c - 2) + at(CDDIFFSQ, r + 2, c + 2)))
        - (GAUSSGRAD[0] * at(SQSUM, r, c)
            + GAUSSGRAD[1] * (at(SQSUM, r - 1, c) + at(SQSUM, r, c + 1) + at(SQSUM, r, c - 1) + at(SQSUM, r + 1, c))
            + GAUSSGRAD[2] * (at(SQSUM, r - 1, c - 1) + at(SQSUM, r - 1, c + 1) + at(SQSUM, r + 1, c - 1) + at(SQSUM, r + 1, c + 1))
            + GAUSSGRAD[3] * (at(SQSUM, r - 2, c) + at(SQSUM, r, c - 2) + at(SQSUM, r, c + 2) + at(SQSUM, r + 2, c))
            + GAUSSGRAD[4] * (at(SQSUM, r - 2, c - 1) + at(SQSUM, r - 2, c + 1) + at(SQSUM, r - 1, c - 2) + at(SQSUM, r - 1, c + 2)
                + at(SQSUM, r + 1, c - 2) + at(SQSUM, r + 1, c + 2) + at(SQSUM, r + 2, c - 1) + at(SQSUM, r + 2, c + 1))
            + GAUSSGRAD[5] * (at(SQSUM, r - 2, c - 2) + at(SQSUM, r - 2, c + 2) + at(SQSUM, r + 2, c - 2) + at(SQSUM, r + 2, c + 2)));
    pput(NYQTEST, r, c, test);
    if test > 0.0 {
        pput(NYQ, r, c, 1.0);
        let slot = id.z * 4u;
        atomicMin(&bounds[slot], u32(r));
        atomicMax(&bounds[slot + 1u], u32(r));
        atomicMin(&bounds[slot + 2u], u32(c));
        atomicMax(&bounds[slot + 3u], u32(c));
    }
}

// Neighbourhood vote on the Nyquist flags: if most neighbours are Nyquist, so
// is this site; on a tie it keeps its flag.
@compute @workgroup_size(64)
fn nyquist_vote(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 8);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    if !in_area(nyquist_area(), r, c) { return; }
    let votes = pat(NYQ, r - 2, c) + pat(NYQ, r - 1, c - 1) + pat(NYQ, r - 1, c + 1)
        + pat(NYQ, r, c - 2) + pat(NYQ, r, c + 2)
        + pat(NYQ, r + 1, c - 1) + pat(NYQ, r + 1, c + 1) + pat(NYQ, r + 2, c);
    var flag = 0.0;
    if votes >= 5.0 {
        flag = 1.0;
    } else if votes == 4.0 {
        flag = pat(NYQ, r, c);
    }
    pput(NYQ2, r, c, flag);
}

// Area interpolation of the direction weight for sites that stay flagged,
// from the CFA statistics of the flagged sites within +-6 samples.
@compute @workgroup_size(64)
fn nyquist_area_weights(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 8);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    if !in_area(nyquist_area(), r, c) || pat(NYQ2, r, c) == 0.0 { return; }

    var sumcfa = 0.0;
    var sumh = 0.0;
    var sumv = 0.0;
    var sumsqh = 0.0;
    var sumsqv = 0.0;
    var areawt = 0.0;
    for (var dr = -6; dr < 7; dr += 2) {
        for (var dc = -6; dc < 7; dc += 2) {
            let rr = r + dr;
            let cc = c + dc;
            if pat(NYQ2, rr, cc) != 0.0 {
                let centre = at(CFA, rr, cc);
                sumcfa += centre;
                sumh += at(CFA, rr, cc - 1) + at(CFA, rr, cc + 1);
                sumv += at(CFA, rr - 1, cc) + at(CFA, rr + 1, cc);
                sumsqh += sqr(centre - at(CFA, rr, cc - 1)) + sqr(centre - at(CFA, rr, cc + 1));
                sumsqv += sqr(centre - at(CFA, rr - 1, cc)) + sqr(centre - at(CFA, rr + 1, cc));
                areawt += 1.0;
            }
        }
    }

    // Horizontal and vertical colour differences, and adaptive weight.
    sumh = sumcfa - 0.5 * sumh;
    sumv = sumcfa - 0.5 * sumv;
    areawt *= 0.5;
    let hcdvar = EPSSQ + abs(areawt * sumsqh - sumh * sumh);
    let vcdvar = EPSSQ + abs(areawt * sumsqv - sumv * sumv);
    pput(HVWT, r, c, hcdvar / (vcdvar + hcdvar));
}

// Final green at red/blue sites from the colour differences and `hvwt`, after
// letting `hvwt` follow the diagonal neighbours where they discriminate
// better. Ordered kernel: the CPU walks the rows in order and updates `hvwt` in
// place, so row r sees the final weights of row r - 1 and the original weights
// of row r + 1. One invocation per pair; the previous row's final weights are
// handed over in workgroup memory, one barrier per row.
@compute @workgroup_size(128)
fn populate_green(@builtin(local_invocation_id) local: vec3<u32>, @builtin(workgroup_id) group: vec3<u32>) {
    base = group.z * TILE_WORDS;
    let q = i32(local.x);
    // Row 7 is never updated: its original weights are the first "previous row".
    chain[128u + local.x] = pat(HVWT, 7, 2 * q);
    workgroupBarrier();
    for (var r = 8; r < TSI - 8; r++) {
        let c = 2 * q + green_at_even(r);
        var weight = pat(HVWT, r, c);
        if q >= 4 && q < (TSI - 8) / 2 {
            // Does the weight of the nearby red/blue sites discriminate better
            // (further from 0.5)?
            let previous = u32((r - 1) & 1) * 128u;
            let alt = (chain[previous + u32((c - 1) >> 1u)] + chain[previous + u32((c + 1) >> 1u)]
                + pat(HVWT, r + 1, c - 1) + pat(HVWT, r + 1, c + 1)) / 4.0;
            if abs(0.5 - weight) < abs(0.5 - alt) { weight = alt; }
            pput(HVWT, r, c, weight);

            // Evaluate the colour difference and G (finally!).
            let dgrb = intp(weight, at(VCD, r, c), at(HCD, r, c));
            let g = at(CFA, r, c) + dgrb;
            put(GREEN, r, c, g);
            pput(DGRB0, r, c, dgrb);

            // Local curvature in G (preparation for the Nyquist refinement),
            // for sites that stayed flagged after the neighbourhood vote.
            // Green's horizontal and vertical neighbours are green sites.
            let flagged = pat(NYQ2, r, c) != 0.0;
            pput(DGRB2H, r, c, select(0.0, sqr(g - 0.5 * (at(GREEN, r, c - 1) + at(GREEN, r, c + 1))), flagged));
            pput(DGRB2V, r, c, select(0.0, sqr(g - 0.5 * (at(GREEN, r - 1, c) + at(GREEN, r + 1, c))), flagged));
        }
        chain[u32(r & 1) * 128u + local.x] = weight;
        workgroupBarrier();
    }
}

// Refine green inside Nyquist areas using the local G curvature, averaged over
// the Nyquist sites of a quincunx neighbourhood.
fn curvature_average(plane: u32, r: i32, c: i32) -> f32 {
    return EPSSQ + (GQUINC[0] * pat(plane, r, c)
        + GQUINC[1] * (pat(plane, r - 1, c - 1) + pat(plane, r - 1, c + 1) + pat(plane, r + 1, c - 1) + pat(plane, r + 1, c + 1))
        + GQUINC[2] * (pat(plane, r - 2, c) + pat(plane, r, c - 2) + pat(plane, r, c + 2) + pat(plane, r + 2, c))
        + GQUINC[3] * (pat(plane, r - 2, c - 2) + pat(plane, r - 2, c + 2) + pat(plane, r + 2, c - 2) + pat(plane, r + 2, c + 2)));
}

@compute @workgroup_size(64)
fn nyquist_refine_green(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 8);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    if !in_area(nyquist_area(), r, c) || pat(NYQ2, r, c) == 0.0 { return; }
    let gvarh = curvature_average(DGRB2H, r, c);
    let gvarv = curvature_average(DGRB2V, r, c);
    // Use the results as weights for the refined G interpolation.
    let dgrb = (at(HCD, r, c) * gvarv + at(VCD, r, c) * gvarh) / (gvarv + gvarh);
    pput(DGRB0, r, c, dgrb);
    put(GREEN, r, c, at(CFA, r, c) + dgrb);
}
