/// The aspect constraint chosen by the crop authoring UI. It is intentionally
/// not stored in the recipe: only the resulting rectangle changes pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CropAspect {
    #[default]
    Free,
    Original,
    Square,
    ThreeTwo,
    FourThree,
    FourFive,
    FiveSeven,
    SixteenNine,
}

impl CropAspect {
    pub const ALL: [Self; 8] = [
        Self::Free,
        Self::Original,
        Self::Square,
        Self::ThreeTwo,
        Self::FourThree,
        Self::FourFive,
        Self::FiveSeven,
        Self::SixteenNine,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Original => "Original",
            Self::Square => "1:1",
            Self::ThreeTwo => "3:2",
            Self::FourThree => "4:3",
            Self::FourFive => "4:5",
            Self::FiveSeven => "5:7",
            Self::SixteenNine => "16:9",
        }
    }

    /// Width over height, in pixels.
    pub(super) const fn ratio(self, original_ratio: f64, portrait: bool) -> Option<f64> {
        let landscape = match self {
            Self::Free => return None,
            Self::Original => original_ratio,
            Self::Square => 1.0,
            Self::ThreeTwo => 3.0 / 2.0,
            Self::FourThree => 4.0 / 3.0,
            Self::FourFive => 4.0 / 5.0,
            Self::FiveSeven => 5.0 / 7.0,
            Self::SixteenNine => 16.0 / 9.0,
        };
        Some(if portrait { 1.0 / landscape } else { landscape })
    }
}
