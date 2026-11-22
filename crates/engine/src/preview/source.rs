use rohditor_demosaic::DemosaicAlgorithm;

/// Where the displayed preview frame came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    Embedded,
    FastCpu,
    FastGpu,
    HighQualityCpu,
    HighQualityGpu,
    OneToOneCpu,
    OneToOneGpu,
}

impl PreviewSource {
    pub const fn developed(algorithm: DemosaicAlgorithm, gpu: bool) -> Self {
        match (algorithm, gpu) {
            (DemosaicAlgorithm::Bilinear, false) => Self::FastCpu,
            (DemosaicAlgorithm::Bilinear, true) => Self::FastGpu,
            (DemosaicAlgorithm::MalvarHeCutler, false) => Self::HighQualityCpu,
            (DemosaicAlgorithm::MalvarHeCutler, true) => Self::HighQualityGpu,
            (DemosaicAlgorithm::Rcd, false) => Self::HighQualityCpu,
            (DemosaicAlgorithm::Rcd, true) => Self::HighQualityGpu,
            (DemosaicAlgorithm::Amaze, false) => Self::HighQualityCpu,
            (DemosaicAlgorithm::Amaze, true) => Self::HighQualityGpu,
            (DemosaicAlgorithm::Neural, false) => Self::HighQualityCpu,
            (DemosaicAlgorithm::Neural, true) => Self::HighQualityGpu,
        }
    }

    /// Anything but the embedded JPEG: the RAW was developed by Rohditor.
    pub const fn is_developed(self) -> bool {
        !matches!(self, Self::Embedded)
    }

    pub const fn is_source_scale(self) -> bool {
        matches!(self, Self::OneToOneCpu | Self::OneToOneGpu)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Embedded => "EMBEDDED PREVIEW · DEVELOPING RAW",
            Self::FastCpu => "FAST ANTIALIASED PREVIEW · CPU",
            Self::FastGpu => "FAST ANTIALIASED PREVIEW · GPU",
            Self::HighQualityCpu => "HIGH-QUALITY ANTIALIASED PREVIEW · CPU",
            Self::HighQualityGpu => "HIGH-QUALITY ANTIALIASED PREVIEW · GPU",
            Self::OneToOneCpu => "1:1 SOURCE-SCALE DEVELOPED · CPU",
            Self::OneToOneGpu => "1:1 SOURCE-SCALE DEVELOPED · GPU",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Embedded => "Embedded preview",
            Self::FastCpu => "Fast preview · CPU",
            Self::FastGpu => "Fast preview · GPU",
            Self::HighQualityCpu => "High-quality preview · CPU",
            Self::HighQualityGpu => "High-quality preview · GPU",
            Self::OneToOneCpu => "1:1 source-scale · CPU",
            Self::OneToOneGpu => "1:1 source-scale · GPU",
        }
    }
}
