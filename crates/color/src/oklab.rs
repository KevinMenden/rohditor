//! OkLab conversions, shared by gamut mapping and colour-range masks.

/// Linear sRGB to OkLab `[L, a, b]`. Out-of-gamut and negative inputs are accepted.
#[must_use]
pub fn linear_srgb_to_oklab([red, green, blue]: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_995 * blue;
    let m = 0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue;
    let s = 0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue;
    let l_root = l.cbrt();
    let m_root = m.cbrt();
    let s_root = s.cbrt();
    [
        0.210_454_26 * l_root + 0.793_617_8 * m_root - 0.004_072_047 * s_root,
        1.977_998_5 * l_root - 2.428_592_2 * m_root + 0.450_593_7 * s_root,
        0.025_904_037 * l_root + 0.782_771_77 * m_root - 0.808_675_77 * s_root,
    ]
}

/// OkLab `[L, a, b]` to linear sRGB; the inverse of [`linear_srgb_to_oklab`].
#[must_use]
pub fn oklab_to_linear_srgb([lightness, a, b]: [f32; 3]) -> [f32; 3] {
    let l_root = lightness + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_root = lightness - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_root = lightness - 0.089_484_18 * a - 1.291_485_5 * b;
    let l = l_root * l_root * l_root;
    let m = m_root * m_root * m_root;
    let s = s_root * s_root * s_root;
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}
