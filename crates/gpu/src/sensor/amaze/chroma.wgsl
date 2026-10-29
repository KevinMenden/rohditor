// Port of amaze/chroma.rs fused with the CPU's `write_tile`: fancy chrominance
// interpolation, then the final red/blue planes written straight to the camera
// planes (the CPU's `red`/`blue` scratch planes are not materialised).

// Split G - B out of the combined colour difference at the blue sites: blue
// rows from row 12 and pairs 6..122 move `dgrb[0]` to `dgrb[1]`.
@compute @workgroup_size(64)
fn chroma_split(@builtin(global_invocation_id) id: vec3<u32>) {
    base = id.z * TILE_WORDS;
    let r = i32(id.x / 128u);
    let q = i32(id.x % 128u);
    if r < 12 || r >= TSI - 12 || q < 6 || q >= (TSI - 12) / 2 { return; }
    let c = 2 * q + green_at_even(r);
    if fc(r, c) != 2u { return; }
    pput(DGRB1, r, c, pat(DGRB0, r, c));
    pput(DGRB0, r, c, 0.0);
}

// Interpolate G - B at red sites and G - R at blue sites from the diagonal
// neighbours, weighting each diagonal by its local smoothness. The plane read
// is the one written only at the other colour's rows, so this is race free.
@compute @workgroup_size(64)
fn chroma_interpolate(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pairs(id, 14);
    if pos.x < 0 { return; }
    let r = pos.x;
    let c = 2 * pos.y + green_at_even(r);
    // G - B (plane 1) at red sites, G - R (plane 0) at blue sites.
    let plane = select(DGRB0, DGRB1, fc(r, c) == 0u);

    let nw1 = pat(plane, r - 1, c - 1);
    let ne1 = pat(plane, r - 1, c + 1);
    let sw1 = pat(plane, r + 1, c - 1);
    let se1 = pat(plane, r + 1, c + 1);
    let nw3 = pat(plane, r - 3, c - 3);
    let ne3 = pat(plane, r - 3, c + 3);
    let sw3 = pat(plane, r + 3, c - 3);
    let se3 = pat(plane, r + 3, c + 3);

    let wtnw = 1.0 / (EPS + abs(nw1 - se1) + abs(nw1 - nw3) + abs(se1 - nw3));
    let wtne = 1.0 / (EPS + abs(ne1 - sw1) + abs(ne1 - ne3) + abs(sw1 - ne3));
    let wtsw = 1.0 / (EPS + abs(sw1 - ne1) + abs(sw1 - se3) + abs(ne1 - sw3));
    let wtse = 1.0 / (EPS + abs(se1 - nw1) + abs(se1 - sw3) + abs(nw1 - se3));

    // Each diagonal contributes its nearest value, extrapolated from the three
    // further ones along it and across it.
    let value = (wtnw * (1.325 * nw1 - 0.175 * nw3 - 0.075 * pat(plane, r - 1, c - 3) - 0.075 * pat(plane, r - 3, c - 1))
        + wtne * (1.325 * ne1 - 0.175 * ne3 - 0.075 * pat(plane, r - 1, c + 3) - 0.075 * se1)
        + wtsw * (1.325 * sw1 - 0.175 * sw3 - 0.075 * pat(plane, r + 1, c - 3) - 0.075 * nw1)
        + wtse * (1.325 * se1 - 0.175 * se3 - 0.075 * pat(plane, r + 1, c + 3) - 0.075 * pat(plane, r + 3, c + 1)))
        / (wtnw + wtne + wtsw + wtse);
    pput(plane, r, c, value);
}

// Reconstructed (red, blue) at a green site: the four neighbouring colour
// differences, weighted by the direction weight.
fn blend(plane: u32, r: i32, c: i32, up: f32, down: f32, left: f32, right: f32, temp: f32) -> f32 {
    return (up * pat(plane, r - 1, c) + (1.0 - right) * pat(plane, r, c + 1)
        + (1.0 - left) * pat(plane, r, c - 1) + down * pat(plane, r + 1, c)) * temp;
}

// Red and blue for the tile's valid interior. At red/blue sites they follow
// directly from green and the colour differences; at green sites the four
// neighbouring differences are averaged with the direction weight. Like
// upstream, reconstructed values are clamped at zero. The measured sample
// replaces its channel's value, green is clamped at zero, and the inverse
// channel balance applies to all three channels (so the measured sample goes
// through `sample * forward * inverse`, as on the CPU).
//
// The pixel is written iff the CPU's `AmazeTileGeometry::write_window` contains
// it (see `write_limit`): the core of this tile minus the image's 16 px bilinear
// ring.
@compute @workgroup_size(64)
fn finish_scatter(@builtin(global_invocation_id) id: vec3<u32>) {
    let pos = begin_pixels(id);
    let r = pos.y;
    let c = pos.x;
    if r < 16 || r >= TSI - 16 || c < 16 || c >= TSI - 16 { return; }
    let origin = vec2<i32>(params.tiles[id.z].xy);
    let global = origin + vec2<i32>(c - 16, r - 16);
    let limit = write_limit(origin);
    if global.x < 16 || global.y < 16 || global.x >= limit.x || global.y >= limit.y { return; }

    let g = at(GREEN, r, c);
    var rgb: vec3<f32>;
    if fc(r, c) == 1u {
        let up = pat(HVWT, r - 1, c);
        let down = pat(HVWT, r + 1, c);
        let left = pat(HVWT, r, c - 1);
        let right = pat(HVWT, r, c + 1);
        let temp = 1.0 / (up + 2.0 - right - left + down);
        rgb = vec3(
            fmax(g - blend(DGRB0, r, c, up, down, left, right, temp), 0.0),
            at(CFA, r, c),
            fmax(g - blend(DGRB1, r, c, up, down, left, right, temp), 0.0));
    } else {
        rgb = vec3(
            fmax(g - pat(DGRB0, r, c), 0.0),
            fmax(g, 0.0),
            fmax(g - pat(DGRB1, r, c), 0.0));
        rgb[fc(r, c)] = at(CFA, r, c);
    }
    rgb *= params.inverse.xyz;
    if any((bitcast<vec3<u32>>(rgb) & vec3(0x7f800000u)) == vec3(0x7f800000u)) {
        atomicOr(&invalid, 1u);
    }
    let local = vec2<i32>(vec2<u32>(global) % params.destination.xy);
    let layer = i32((u32(global.y) / params.destination.y) * params.destination.z + u32(global.x) / params.destination.x);
    textureStore(red, local, layer, vec4(rgb.r, 0.0, 0.0, 1.0));
    textureStore(green, local, layer, vec4(rgb.g, 0.0, 0.0, 1.0));
    textureStore(blue, local, layer, vec4(rgb.b, 0.0, 0.0, 1.0));
}
