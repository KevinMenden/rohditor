//! Ultra HDR gain-map XMP (`hdrgm:` namespace, version 1.0).
//!
//! This is the XMP packet that describes the *gain-map image*. The primary image's own XMP
//! (the container directory pointing at the gain map) is part of container assembly and not
//! produced here. The result is the bare `x:xmpmeta` element; the JPEG APP1 signature and
//! length framing are the container writer's job.

use std::fmt::Write;

use crate::{GainMapError, GainMapMetadata};

/// Namespace URI bound to the `hdrgm:` prefix.
pub const HDRGM_NAMESPACE: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";

/// Serialises single-channel metadata. Ultra HDR spells multi-channel values as RDF
/// sequences, which v1 does not write, so three channels are rejected.
pub fn to_string(metadata: &GainMapMetadata) -> Result<String, GainMapError> {
    metadata.validate()?;
    let [channel] = metadata.channels.as_slice() else {
        return Err(GainMapError::UnsupportedChannelCount(
            metadata.channels.len(),
        ));
    };
    let mut xmp = String::new();
    // Writing to a String cannot fail.
    let _ = write!(
        xmp,
        concat!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Adobe XMP Core 5.1.2\">",
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">",
            "<rdf:Description rdf:about=\"\" xmlns:hdrgm=\"{ns}\"",
            " hdrgm:Version=\"1.0\"",
            " hdrgm:GainMapMin=\"{min}\"",
            " hdrgm:GainMapMax=\"{max}\"",
            " hdrgm:Gamma=\"{gamma}\"",
            " hdrgm:OffsetSDR=\"{offset_sdr}\"",
            " hdrgm:OffsetHDR=\"{offset_hdr}\"",
            " hdrgm:HDRCapacityMin=\"{capacity_min}\"",
            " hdrgm:HDRCapacityMax=\"{capacity_max}\"",
            " hdrgm:BaseRenditionIsHDR=\"False\"/>",
            "</rdf:RDF></x:xmpmeta>"
        ),
        ns = HDRGM_NAMESPACE,
        min = channel.min_log2,
        max = channel.max_log2,
        gamma = channel.gamma,
        offset_sdr = channel.offset_sdr,
        offset_hdr = channel.offset_hdr,
        capacity_min = metadata.base_hdr_headroom_log2,
        capacity_max = metadata.alternate_hdr_headroom_log2,
    );
    Ok(xmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMetadata;

    #[test]
    fn writes_all_hdrgm_attributes() {
        let metadata = GainMapMetadata::single_channel(ChannelMetadata::new(0.0, 2.5), 2.5);
        let xmp = to_string(&metadata).unwrap();
        for attribute in [
            "hdrgm:Version=\"1.0\"",
            "hdrgm:GainMapMin=\"0\"",
            "hdrgm:GainMapMax=\"2.5\"",
            "hdrgm:Gamma=\"1\"",
            "hdrgm:OffsetSDR=\"0.015625\"",
            "hdrgm:OffsetHDR=\"0.015625\"",
            "hdrgm:HDRCapacityMin=\"0\"",
            "hdrgm:HDRCapacityMax=\"2.5\"",
            "hdrgm:BaseRenditionIsHDR=\"False\"",
            "xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\"",
        ] {
            assert!(xmp.contains(attribute), "missing {attribute} in {xmp}");
        }
        assert!(xmp.starts_with("<x:xmpmeta") && xmp.ends_with("</x:xmpmeta>"));
    }

    #[test]
    fn rejects_invalid_and_multichannel_metadata() {
        let mut metadata = GainMapMetadata::single_channel(ChannelMetadata::new(0.0, 3.0), 3.0);
        metadata.channels[0].gamma = -1.0;
        assert!(to_string(&metadata).is_err());
        let channel = ChannelMetadata::new(0.0, 3.0);
        let rgb = GainMapMetadata {
            channels: vec![channel; 3],
            base_hdr_headroom_log2: 0.0,
            alternate_hdr_headroom_log2: 3.0,
        };
        assert_eq!(
            to_string(&rgb),
            Err(GainMapError::UnsupportedChannelCount(3))
        );
    }
}
