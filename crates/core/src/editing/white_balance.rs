use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{TEMPERATURE_RANGE, TINT_RANGE, WHITE_BALANCE_MULTIPLIER_RANGE, WhiteBalance};

use crate::{Matrix3, WhiteBalanceCoordinates, coordinates_from_camera_gains_relative_to_as_shot};

/// Which white-balance parameterization is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteBalanceMode {
    AsShot,
    TemperatureTint,
    ManualMultipliers,
}

/// Keep the last values for the two editable WB modes while the user switches
/// through As-shot. This makes the mode selector non-destructive instead of
/// silently resetting a carefully chosen temperature or manual balance.
#[derive(Debug, Clone, Copy)]
pub struct WhiteBalanceModeMemory {
    temperature: Option<f32>,
    tint: Option<f32>,
    manual: [f32; 3],
}

impl Default for WhiteBalanceModeMemory {
    fn default() -> Self {
        Self {
            temperature: None,
            tint: None,
            manual: [WHITE_BALANCE_MULTIPLIER_RANGE.neutral; 3],
        }
    }
}

impl WhiteBalanceModeMemory {
    pub fn remember(&mut self, balance: WhiteBalance) {
        match balance {
            WhiteBalance::TemperatureTint { temperature, tint } => {
                self.temperature = Some(temperature);
                self.tint = Some(tint);
            }
            WhiteBalance::ManualMultipliers { red, green, blue } => {
                self.manual = [red, green, blue];
            }
            WhiteBalance::AsShot => {}
        }
    }

    /// Resolve the balance after switching to `mode`, restoring remembered
    /// values (or the As Shot coordinates) for the editable modes.
    pub fn select(
        &mut self,
        current: WhiteBalance,
        mode: WhiteBalanceMode,
        as_shot: Option<WhiteBalanceCoordinates>,
    ) -> WhiteBalance {
        self.remember(current);
        let as_shot = as_shot.unwrap_or(WhiteBalanceCoordinates {
            temperature: TEMPERATURE_RANGE.neutral,
            tint: TINT_RANGE.neutral,
        });
        match mode {
            WhiteBalanceMode::AsShot => WhiteBalance::AsShot,
            WhiteBalanceMode::TemperatureTint => match current {
                WhiteBalance::TemperatureTint { temperature, tint } => {
                    WhiteBalance::TemperatureTint { temperature, tint }
                }
                _ => WhiteBalance::TemperatureTint {
                    temperature: self.temperature.unwrap_or(as_shot.temperature),
                    tint: self.tint.unwrap_or(as_shot.tint),
                },
            },
            WhiteBalanceMode::ManualMultipliers => match current {
                WhiteBalance::ManualMultipliers { red, green, blue } => {
                    WhiteBalance::ManualMultipliers { red, green, blue }
                }
                _ => WhiteBalance::ManualMultipliers {
                    red: self.manual[0],
                    green: self.manual[1],
                    blue: self.manual[2],
                },
            },
        }
    }
}

/// White balance that makes a camera-native RGB patch neutral, relative to the
/// source's As Shot gains. Prefers Temperature/Tint and falls back to manual
/// multipliers; `None` when the sample or metadata cannot be represented.
#[must_use]
pub fn white_balance_from_camera_sample(
    sample: [f32; 3],
    unbalanced_camera_to_xyz: Matrix3,
    as_shot_white_balance: Option<[Option<f32>; 4]>,
) -> Option<WhiteBalance> {
    if sample
        .iter()
        .any(|value| !value.is_finite() || *value <= 1.0e-5)
    {
        return None;
    }
    let desired_total = [sample[1] / sample[0], 1.0, sample[1] / sample[2]];
    let gains = WhiteBalanceGains {
        red: desired_total[0],
        green: desired_total[1],
        blue: desired_total[2],
    };
    let [red, green, blue, _] = as_shot_white_balance?;
    let [Some(red), Some(green), Some(blue)] = [red, green, blue]
        .map(|value| value.filter(|number| number.is_finite() && *number > 1.0e-5))
    else {
        return None;
    };
    let as_shot_relative = [red / green, 1.0, blue / green];
    let as_shot_gains = WhiteBalanceGains {
        red: as_shot_relative[0],
        green: as_shot_relative[1],
        blue: as_shot_relative[2],
    };
    if let Ok(coordinates) = coordinates_from_camera_gains_relative_to_as_shot(
        unbalanced_camera_to_xyz,
        as_shot_gains,
        gains,
    ) {
        return Some(WhiteBalance::TemperatureTint {
            temperature: coordinates.temperature,
            tint: coordinates.tint,
        });
    }

    // If the sampled neutral lies outside the calibrated locus, preserve the
    // exact camera-native result with the existing relative multiplier mode.
    // This is a bounded fallback for unusual sensors or strongly coloured
    // patches.
    let manual = [
        desired_total[0] / as_shot_relative[0],
        WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
        desired_total[2] / as_shot_relative[2],
    ];
    manual
        .iter()
        .all(|value| WHITE_BALANCE_MULTIPLIER_RANGE.contains(*value))
        .then_some(WhiteBalance::ManualMultipliers {
            red: manual[0],
            green: manual[1],
            blue: manual[2],
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_uses_camera_native_channel_ratios() {
        let neutral = white_balance_from_camera_sample(
            [0.5, 0.5, 0.5],
            Matrix3::identity(),
            Some([Some(1.0), Some(1.0), Some(1.0), None]),
        )
        .expect("neutral sample");
        let neutral_gains = match neutral {
            WhiteBalance::ManualMultipliers { red, green, blue } => {
                WhiteBalanceGains { red, green, blue }
            }
            WhiteBalance::TemperatureTint { temperature, tint } => {
                crate::camera_gains_from_as_shot_coordinates(
                    Matrix3::identity(),
                    WhiteBalanceGains::identity(),
                    WhiteBalanceCoordinates { temperature, tint },
                )
                .expect("temperature/tint gains")
            }
            WhiteBalance::AsShot => panic!("picker should not leave the balance as shot"),
        };
        assert!((neutral_gains.red - 1.0).abs() < 0.01);
        assert!((neutral_gains.green - 1.0).abs() < 0.01);
        assert!((neutral_gains.blue - 1.0).abs() < 0.01);

        let colored = white_balance_from_camera_sample(
            [0.8, 0.5, 0.2],
            Matrix3::identity(),
            Some([Some(1.0), Some(1.0), Some(1.0), None]),
        )
        .expect("colored sample");
        let gains = match colored {
            WhiteBalance::TemperatureTint { temperature, tint } => {
                crate::camera_gains_from_as_shot_coordinates(
                    Matrix3::identity(),
                    WhiteBalanceGains::identity(),
                    WhiteBalanceCoordinates { temperature, tint },
                )
                .expect("temperature/tint gains")
            }
            WhiteBalance::ManualMultipliers { red, green, blue } => {
                WhiteBalanceGains { red, green, blue }
            }
            WhiteBalance::AsShot => panic!("picker should not leave the balance as shot"),
        };
        assert!((gains.red - 0.625).abs() < 0.01);
        assert!((gains.blue - 2.5).abs() < 0.01);
        assert!(
            white_balance_from_camera_sample(
                [0.0, 0.5, 0.5],
                Matrix3::identity(),
                Some([Some(1.0), Some(1.0), Some(1.0), None])
            )
            .is_none()
        );
    }

    #[test]
    fn picker_accounts_for_as_shot_baseline() {
        let gains = match white_balance_from_camera_sample(
            [0.4, 0.5, 0.6],
            Matrix3::identity(),
            Some([Some(2.0), Some(1.0), Some(1.5), None]),
        )
        .expect("sample should fit the balance range")
        {
            WhiteBalance::TemperatureTint { temperature, tint } => {
                crate::camera_gains_from_as_shot_coordinates(
                    Matrix3::identity(),
                    WhiteBalanceGains {
                        red: 2.0,
                        green: 1.0,
                        blue: 1.5,
                    },
                    WhiteBalanceCoordinates { temperature, tint },
                )
                .expect("temperature/tint gains")
            }
            WhiteBalance::ManualMultipliers { red, green, blue } => WhiteBalanceGains {
                red: 2.0 * red,
                green,
                blue: 1.5 * blue,
            },
            WhiteBalance::AsShot => panic!("picker should not leave the balance as shot"),
        };
        assert!((gains.red - 1.25).abs() < 0.01);
        assert!((gains.blue - (5.0 / 6.0)).abs() < 0.01);
    }

    #[test]
    fn mode_switch_preserves_last_editable_values() {
        let mut memory = WhiteBalanceModeMemory::default();
        let temperature = WhiteBalance::TemperatureTint {
            temperature: 7_400.0,
            tint: -0.35,
        };
        let manual = WhiteBalance::ManualMultipliers {
            red: 1.6,
            green: 0.8,
            blue: 1.2,
        };

        let current = memory.select(temperature, WhiteBalanceMode::ManualMultipliers, None);
        assert_eq!(
            current,
            WhiteBalance::ManualMultipliers {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
            }
        );
        // The user edits the manual values, then moves through As Shot.
        let current = memory.select(manual, WhiteBalanceMode::AsShot, None);
        assert_eq!(current, WhiteBalance::AsShot);

        let current = memory.select(current, WhiteBalanceMode::TemperatureTint, None);
        assert_eq!(current, temperature);
        let current = memory.select(current, WhiteBalanceMode::ManualMultipliers, None);
        assert_eq!(current, manual);
    }

    #[test]
    fn temperature_tint_starts_at_the_document_as_shot_coordinates() {
        let mut memory = WhiteBalanceModeMemory::default();
        let selected = memory.select(
            WhiteBalance::AsShot,
            WhiteBalanceMode::TemperatureTint,
            Some(WhiteBalanceCoordinates {
                temperature: 5_100.0,
                tint: -0.18,
            }),
        );
        assert_eq!(
            selected,
            WhiteBalance::TemperatureTint {
                temperature: 5_100.0,
                tint: -0.18,
            }
        );
    }
}
