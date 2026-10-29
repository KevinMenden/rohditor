// Port of amaze/green.rs plus the tile population: gradients, colour
// differences, bounding and the direction weight `hvwt`.

// CPU `populate`: fill the CFA plane from the mosaic (mirrored beyond the image
// edge, scaled by the channel balance) and reset what the stages expect to find
// zeroed. `vcd`/`hcd`/`vcdalt`/`hcdalt` are read up to two samples outside the
// region the stages write, so their 4 px outer ring is cleared; `dgrb2_*` and
// the Nyquist flags are read where nothing writes them.
@compute @workgroup_size(64)
fn clear_populate(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pixels(id);
    let r = pos.y;
    let c = pos.x;
    let slot = id.z;
    if id.x == 0u {
        atomicStore(&bounds[slot * 4u], 0xffffffffu);
        atomicStore(&bounds[slot * 4u + 1u], 0u);
        atomicStore(&bounds[slot * 4u + 2u], 0xffffffffu);
        atomicStore(&bounds[slot * 4u + 3u], 0u);
    }
    if r < 4 || r >= TSI - 4 || c < 4 || c >= TSI - 4 {
        put(VCD, r, c, 0.0);
        put(HCD, r, c, 0.0);
        put(VCDALT, r, c, 0.0);
        put(HCDALT, r, c, 0.0);
    }
    if (c & 1) == 0 {
        pput(DGRB2H, r, c, 0.0);
        pput(DGRB2V, r, c, 0.0);
        pput(NYQ, r, c, 0.0);
        pput(NYQ2, r, c, 0.0);
    }
    let origin = vec2<i32>(params.tiles[slot].xy) - 16;
    let source = vec2<u32>(
        mirror(origin.x + c, i32(params.image.x)),
        mirror(origin.y + r, i32(params.image.y)));
    let value = sample(source) * params.forward[fc(r, c)];
    put(CFA, r, c, value);
    put(GREEN, r, c, value);
}

// Direction weights and gradient energy from the raw CFA.
@compute @workgroup_size(64)
fn gradients(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pixels(id);
    let r = pos.y;
    let c = pos.x;
    if r < 2 || r >= TSI - 2 || c < 2 || c >= TSI - 2 { return; }
    let delh = abs(at(CFA, r, c + 1) - at(CFA, r, c - 1));
    let delv = abs(at(CFA, r + 1, c) - at(CFA, r - 1, c));
    put(W0, r, c, EPS + abs(at(CFA, r + 2, c) - at(CFA, r, c)) + abs(at(CFA, r, c) - at(CFA, r - 2, c)) + delv);
    put(W1, r, c, EPS + abs(at(CFA, r, c + 2) - at(CFA, r, c)) + abs(at(CFA, r, c) - at(CFA, r, c - 2)) + delh);
    put(SQSUM, r, c, sqr(delh) + sqr(delv));
}

// Green estimate from the adaptive ratio when it is plausible, else the
// Hamilton-Adams fallback.
fn adaptive_or(ratio: f32, centre: f32, fallback: f32) -> f32 {
    return select(fallback, centre * ratio, abs(1.0 - ratio) < ARTHRESH);
}

// Vertical and horizontal colour differences (G - R or G - B) using adaptive
// ratios, plus the Hamilton-Adams alternative.
@compute @workgroup_size(64)
fn colour_differences(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pixels(id);
    let r = pos.y;
    let c = pos.x;
    if r < 4 || r >= TSI - 4 || c < 4 || c >= TSI - 4 { return; }
    let clip_pt8 = params.clip.y;
    let at_green = fc(r, c) == 1u;
    let cur = at(CFA, r, c);
    let up = at(CFA, r - 1, c);
    let up2 = at(CFA, r - 2, c);
    let down = at(CFA, r + 1, c);
    let down2 = at(CFA, r + 2, c);
    let left = at(CFA, r, c - 1);
    let left2 = at(CFA, r, c - 2);
    let right = at(CFA, r, c + 1);
    let right2 = at(CFA, r, c + 2);

    let w0_up2 = at(W0, r - 2, c);
    let w0_up = at(W0, r - 1, c);
    let w0_mid = at(W0, r, c);
    let w0_down = at(W0, r + 1, c);
    let w0_down2 = at(W0, r + 2, c);
    let w1_left2 = at(W1, r, c - 2);
    let w1_left = at(W1, r, c - 1);
    let w1_mid = at(W1, r, c);
    let w1_right = at(W1, r, c + 1);
    let w1_right2 = at(W1, r, c + 2);

    // Colour ratios in each cardinal direction.
    let cru = up * (w0_up2 + w0_mid) / (w0_up2 * (EPS + cur) + w0_mid * (EPS + up2));
    let crd = down * (w0_down2 + w0_mid) / (w0_down2 * (EPS + cur) + w0_mid * (EPS + down2));
    let crl = left * (w1_left2 + w1_mid) / (w1_left2 * (EPS + cur) + w1_mid * (EPS + left2));
    let crr = right * (w1_right2 + w1_mid) / (w1_right2 * (EPS + cur) + w1_mid * (EPS + right2));

    // G interpolated in vertical/horizontal directions, Hamilton-Adams.
    let guha = up + 0.5 * (cur - up2);
    let gdha = down + 0.5 * (cur - down2);
    let glha = left + 0.5 * (cur - left2);
    let grha = right + 0.5 * (cur - right2);

    // G interpolated using adaptive ratios.
    var guar = adaptive_or(cru, cur, guha);
    var gdar = adaptive_or(crd, cur, gdha);
    var glar = adaptive_or(crl, cur, glha);
    var grar = adaptive_or(crr, cur, grha);

    // Adaptive weights for vertical/horizontal directions.
    let hwt = w1_left / (w1_left + w1_right);
    let vwt = w0_up / (w0_down + w0_up);

    let gintvha = vwt * gdha + (1.0 - vwt) * guha;
    let ginthha = hwt * grha + (1.0 - hwt) * glha;

    // Every entry is G - (R or B): the sign flips between green and red/blue.
    var vcd: f32;
    var hcd: f32;
    var vcdalt: f32;
    var hcdalt: f32;
    if at_green {
        vcd = cur - (vwt * gdar + (1.0 - vwt) * guar);
        hcd = cur - (hwt * grar + (1.0 - hwt) * glar);
        vcdalt = cur - gintvha;
        hcdalt = cur - ginthha;
    } else {
        vcd = (vwt * gdar + (1.0 - vwt) * guar) - cur;
        hcd = (hwt * grar + (1.0 - hwt) * glar) - cur;
        vcdalt = gintvha - cur;
        hcdalt = ginthha - cur;
    }

    // Use Hamilton-Adams if highlights are (nearly) clipped.
    let clipped = (cur > clip_pt8) || (gintvha > clip_pt8) || (ginthha > clip_pt8);
    if clipped {
        guar = guha;
        gdar = gdha;
        glar = glha;
        grar = grha;
        vcd = vcdalt;
        hcd = hcdalt;
    }

    put(DGINTV, r, c, fmin(sqr(guha - gdha), sqr(guar - gdar)));
    put(DGINTH, r, c, fmin(sqr(glha - grha), sqr(glar - grar)));
    put(VCD, r, c, vcd);
    put(HCD, r, c, hcd);
    put(VCDALT, r, c, vcdalt);
    put(HCDALT, r, c, hcdalt);
}

// Bound a colour difference `d` (G - C) at a green site, where `cur` is the
// green sample and `a`, `b` its two neighbours along the direction.
fn bound_at_green(d: f32, cur: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let g = -d + cur;
    let alt = -median(g, a, b) + cur;
    var bounded = d;
    if d > 0.0 {
        if 3.0 * d > g + cur {
            bounded = alt;
        } else {
            let weight = 1.0 - 3.0 * d / (EPS + g + cur);
            bounded = weight * d + (1.0 - weight) * alt;
        }
    }
    return select(bounded, alt, g > clip_pt);
}

// Bound a colour difference `d` (G - C) at a red/blue site.
fn bound_at_red_blue(d: f32, cur: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let g = d + cur;
    let alt = median(g, a, b) - cur;
    var bounded = d;
    if d < 0.0 {
        if 3.0 * d < -(g + cur) {
            bounded = alt;
        } else {
            let weight = 1.0 + 3.0 * d / (EPS + g + cur);
            bounded = weight * d + (1.0 - weight) * alt;
        }
    }
    return select(bounded, alt, g > clip_pt);
}

fn bound(d: f32, cur: f32, a: f32, b: f32, at_green: bool) -> f32 {
    let clip_pt = params.clip.x;
    if at_green { return bound_at_green(d, cur, a, b, clip_pt); }
    return bound_at_red_blue(d, cur, a, b, clip_pt);
}

// Variance-like measure of three samples: 3 * sum(x^2) - (sum x)^2.
fn spread(a: f32, b: f32, c: f32) -> f32 {
    return 3.0 * (sqr(a) + sqr(b) + sqr(c)) - sqr(a + b + c);
}

// Ordered kernel: the vertical colour difference of row r reads the already
// bounded row r - 2 and the original row r + 2 of its own column, so one
// invocation per column walks the rows in CPU order.
@compute @workgroup_size(64)
fn bound_vcd(@builtin(global_invocation_id) id: vec3<u32>) {
    base = id.z * TILE_WORDS;
    let c = i32(id.x);
    if c < 4 || c >= TSI - 4 { return; }
    for (var r = 4; r < TSI - 4; r++) {
        let vcdvar = spread(at(VCD, r - 2, c), at(VCD, r, c), at(VCD, r + 2, c));
        let vcdaltvar = spread(at(VCDALT, r - 2, c), at(VCDALT, r, c), at(VCDALT, r + 2, c));
        let v = select(at(VCD, r, c), at(VCDALT, r, c), vcdaltvar < vcdvar);
        put(VCD, r, c, bound(v, at(CFA, r, c), at(CFA, r - 1, c), at(CFA, r + 1, c), fc(r, c) == 1u));
    }
}

// Ordered kernel: the horizontal difference chains along its row the same way
// (bounded column c - 2, original column c + 2), one invocation per row.
@compute @workgroup_size(64)
fn bound_hcd(@builtin(global_invocation_id) id: vec3<u32>) {
    base = id.z * TILE_WORDS;
    let r = i32(id.x);
    if r < 4 || r >= TSI - 4 { return; }
    for (var c = 4; c < TSI - 4; c++) {
        let hcdvar = spread(at(HCD, r, c - 2), at(HCD, r, c), at(HCD, r, c + 2));
        let hcdaltvar = spread(at(HCDALT, r, c - 2), at(HCDALT, r, c), at(HCDALT, r, c + 2));
        let h = select(at(HCD, r, c), at(HCDALT, r, c), hcdaltvar < hcdvar);
        put(HCD, r, c, bound(h, at(CFA, r, c), at(CFA, r, c - 1), at(CFA, r, c + 1), fc(r, c) == 1u));
    }
}

// Squared V-H colour difference at red/blue sites, from the bounded values.
@compute @workgroup_size(64)
fn cddiffsq(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pixels(id);
    let r = pos.y;
    let c = pos.x;
    if r < 4 || r >= TSI - 4 || c < 4 || c >= TSI - 4 || fc(r, c) == 1u { return; }
    put(CDDIFFSQ, r, c, sqr(at(VCD, r, c) - at(HCD, r, c)));
}

// Adaptive horizontal-versus-vertical weight at every red/blue site.
@compute @workgroup_size(64)
fn direction_weights(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 6);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);

    // Colour difference variances in the cardinal directions.
    let v0 = at(VCD, r - 3, c);
    let v1 = at(VCD, r - 2, c);
    let v2 = at(VCD, r - 1, c);
    let v3 = at(VCD, r, c);
    let v4 = at(VCD, r + 1, c);
    let v5 = at(VCD, r + 2, c);
    let v6 = at(VCD, r + 3, c);
    let h0 = at(HCD, r, c);
    let hl1 = at(HCD, r, c - 1);
    let hl2 = at(HCD, r, c - 2);
    let hl3 = at(HCD, r, c - 3);
    let hr1 = at(HCD, r, c + 1);
    let hr2 = at(HCD, r, c + 2);
    let hr3 = at(HCD, r, c + 3);
    let uave = v3 + v2 + v1 + v0;
    let dave = v3 + v4 + v5 + v6;
    let lave = h0 + hl1 + hl2 + hl3;
    let rave = h0 + hr1 + hr2 + hr3;

    let dgrbvvaru = sqr(v3 - uave) + sqr(v2 - uave) + sqr(v1 - uave) + sqr(v0 - uave);
    let dgrbvvard = sqr(v3 - dave) + sqr(v4 - dave) + sqr(v5 - dave) + sqr(v6 - dave);
    let dgrbhvarl = sqr(h0 - lave) + sqr(hl1 - lave) + sqr(hl2 - lave) + sqr(hl3 - lave);
    let dgrbhvarr = sqr(h0 - rave) + sqr(hr1 - rave) + sqr(hr2 - rave) + sqr(hr3 - rave);

    let hwt = at(W1, r, c - 1) / (at(W1, r, c - 1) + at(W1, r, c + 1));
    let vwt = at(W0, r - 1, c) / (at(W0, r + 1, c) + at(W0, r - 1, c));

    let vcdvar = EPSSQ + vwt * dgrbvvard + (1.0 - vwt) * dgrbvvaru;
    let hcdvar = EPSSQ + hwt * dgrbhvarr + (1.0 - hwt) * dgrbhvarl;

    // Fluctuations in up/down and left/right interpolations of colours.
    let fluct_u = at(DGINTV, r, c) + at(DGINTV, r - 1, c) + at(DGINTV, r - 2, c);
    let fluct_d = at(DGINTV, r, c) + at(DGINTV, r + 1, c) + at(DGINTV, r + 2, c);
    let fluct_l = at(DGINTH, r, c) + at(DGINTH, r, c - 1) + at(DGINTH, r, c - 2);
    let fluct_r = at(DGINTH, r, c) + at(DGINTH, r, c + 1) + at(DGINTH, r, c + 2);

    let vcdvar1 = EPSSQ + vwt * fluct_d + (1.0 - vwt) * fluct_u;
    let hcdvar1 = EPSSQ + hwt * fluct_r + (1.0 - hwt) * fluct_l;

    // Adaptive weights for the green interpolation.
    let varwt = hcdvar / (vcdvar + hcdvar);
    let diffwt = hcdvar1 / (vcdvar1 + hcdvar1);

    // If both agree on the direction use the one with the strongest
    // discrimination; otherwise use the fluctuation weight.
    let agree = (0.5 - varwt) * (0.5 - diffwt) > 0.0 && abs(0.5 - diffwt) < abs(0.5 - varwt);
    pput(HVWT, r, c, select(diffwt, varwt, agree));
}
