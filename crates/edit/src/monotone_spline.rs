//! Monotone cubic interpolation through `[input, output]` knots.
//!
//! Used by the fitted camera tone and the tone curve, so both bend between
//! their knots the same way and never overshoot or invert tonal order.

/// Evaluate the Fritsch-Carlson spline through `knots` at `input`.
///
/// The knots must be at least two, with strictly increasing inputs and
/// non-decreasing outputs. Inputs outside the first and last knot clamp to
/// their outputs.
pub(crate) fn evaluate(knots: &[[f32; 2]], input: f32) -> f32 {
    let count = knots.len();
    debug_assert!(count >= 2, "a spline needs two knots");
    if input <= knots[0][0] {
        return knots[0][1];
    }
    if input >= knots[count - 1][0] {
        return knots[count - 1][1];
    }
    let segment = knots
        .partition_point(|knot| knot[0] <= input)
        .clamp(1, count - 1)
        - 1;
    let [x0, y0] = knots[segment];
    let [x1, y1] = knots[segment + 1];
    let width = x1 - x0;
    let m0 = tangent(knots, segment);
    let m1 = tangent(knots, segment + 1);
    // Cubic Hermite basis on the unit interval.
    let t = (input - x0) / width;
    let t2 = t * t;
    let t3 = t2 * t;
    let value = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
        + (t3 - 2.0 * t2 + t) * width * m0
        + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * width * m1;
    value.clamp(y0, y1)
}

/// Fritsch-Carlson tangent at one knot. Zero where the neighbouring secants
/// disagree in sign or either is flat, which keeps each segment monotone.
fn tangent(knots: &[[f32; 2]], index: usize) -> f32 {
    let secant = |segment: usize| {
        let [x0, y0] = knots[segment];
        let [x1, y1] = knots[segment + 1];
        (y1 - y0) / (x1 - x0)
    };
    let last = knots.len() - 1;
    if index == 0 {
        return secant(0);
    }
    if index == last {
        return secant(last - 1);
    }
    let (before, after) = (secant(index - 1), secant(index));
    if before * after <= 0.0 {
        return 0.0;
    }
    // Harmonic mean limits overshoot between unevenly spaced knots.
    2.0 * before * after / (before + after)
}
