//! Camera-look matching: the `look-report` measurement command and the fit used
//! by `develop --rendering-profile matched`.
//!
//! The report renders each RAW with Neutral, Standard and (when the fit is
//! accepted) Matched, then compares every rendering with the camera's embedded
//! JPEG on a coarse binned proxy. It is the baseline and acceptance gate for
//! changes to the default look.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rohditor_camera_profile::parse_dcp_file;
use rohditor_core::{
    CpuPipeline, OutputPolicy, PreviewOptions, RenderOptions, decode_camera_preview,
    match_camera_look,
};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, RenderingAdjustments, RenderingProfileSelection,
};
use rohditor_look::{Proxy, ProxyDifference, compare_proxies};
use rohditor_raw::{RawDecoder, RawFrame, RawlerDecoder};
use serde::Serialize;

use super::commands::write_stdout;

/// Long edge of the renders compared with the JPEG. The proxy is binned far
/// below this, so a small render is enough.
const REPORT_RENDER_LONG_EDGE: usize = 768;

/// Fit the camera tone for `frame` and store it in the recipe.
///
/// A rejected or impossible fit is not an error for the caller: the recipe then
/// uses the static fallback look, and the reason is printed.
pub(super) fn fit_recipe_camera_tone(
    decoder: &RawlerDecoder,
    pipeline: &CpuPipeline,
    file: &Path,
    frame: &RawFrame,
    recipe: &mut EditRecipe,
) -> Result<()> {
    let Some(preview) = decoder
        .embedded_preview(file)
        .with_context(|| format!("could not read the embedded preview of {}", file.display()))?
    else {
        eprintln!(
            "No embedded preview in {}; Matched uses its static look",
            file.display()
        );
        return Ok(());
    };
    match match_camera_look(pipeline, frame, &preview, &recipe.color.camera_profile) {
        Ok(look) => {
            eprintln!(
                "Camera tone fitted (distribution error {:.3})",
                look.tone.distribution_error
            );
            match &look.chroma {
                Ok(fit) => eprintln!("Camera chroma fitted (gains {:.2?})", fit.chroma.gains),
                Err(error) => eprintln!("{error}"),
            }
            look.store_in(&mut recipe.rendering);
        }
        Err(error) => eprintln!("{error}; Matched uses its static look"),
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct LookReport {
    schema_version: u32,
    sources: Vec<SourceLook>,
}

#[derive(Debug, Serialize)]
struct SourceLook {
    file: String,
    camera: String,
    /// Tone fit outcome: `accepted` or the rejection reason.
    fit: String,
    distribution_error: Option<f32>,
    /// Fitted tone knots (`[encoded input, encoded output]`), for plotting the
    /// camera's curve or deriving static defaults.
    tone_knots: Option<Vec<[f32; 2]>>,
    /// Chroma fit outcome: `accepted`, the refusal reason, or `not attempted`.
    chroma_fit: String,
    chroma_gains: Option<[f32; 9]>,
    renderings: Vec<RenderingLook>,
}

#[derive(Debug, Serialize)]
struct RenderingLook {
    rendering: &'static str,
    #[serde(flatten)]
    difference: Difference,
}

/// `ProxyDifference` with serializable fields and stable names.
#[derive(Debug, Serialize)]
struct Difference {
    channel_rms: [f32; 3],
    mean_color_error: f32,
    p95_color_error: f32,
    luminance_rms: f32,
    luminance_bias: f32,
}

impl From<ProxyDifference> for Difference {
    fn from(value: ProxyDifference) -> Self {
        Self {
            channel_rms: value.channel_rms,
            mean_color_error: value.mean_color_error,
            p95_color_error: value.p95_color_error,
            luminance_rms: value.luminance_rms,
            luminance_bias: value.luminance_bias,
        }
    }
}

/// Measure every RAW against its embedded JPEG and print the report.
pub(super) fn look_report(
    files: &[std::path::PathBuf],
    camera_profile: Option<&Path>,
    json: bool,
) -> Result<()> {
    let selection = match camera_profile {
        Some(path) => CameraProfileSelection::Matrix(
            parse_dcp_file(path).with_context(|| format!("could not parse {}", path.display()))?,
        ),
        None => CameraProfileSelection::Automatic,
    };
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default();
    let mut sources = Vec::new();
    for file in files {
        let source = measure_source(&decoder, &pipeline, file, &selection)
            .with_context(|| format!("could not measure {}", file.display()))?;
        sources.push(source);
    }
    let report = LookReport {
        schema_version: 1,
        sources,
    };
    if json {
        let text = serde_json::to_string_pretty(&report).context("could not serialize report")?;
        return write_stdout(&text);
    }
    write_stdout(&format_report(&report))
}

fn measure_source(
    decoder: &RawlerDecoder,
    pipeline: &CpuPipeline,
    file: &Path,
    camera_profile: &CameraProfileSelection,
) -> Result<SourceLook> {
    let frame = decoder
        .decode(file)
        .with_context(|| format!("could not decode {}", file.display()))?;
    let Some(preview) = decoder.embedded_preview(file)? else {
        bail!(
            "{} has no embedded preview to compare against",
            file.display()
        );
    };
    let camera = decode_camera_preview(&preview, frame.info.orientation)?;

    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe.color.camera_profile = camera_profile.clone();
    let base = pipeline.prepare_preview_base(
        &frame,
        &recipe,
        PreviewOptions {
            render: RenderOptions::default(),
            max_long_edge: REPORT_RENDER_LONG_EDGE,
        },
    )?;

    let mut chroma_fit = "not attempted".to_owned();
    let mut chroma_gains = None;
    let mut tone_knots = None;
    let (fit_outcome, distribution_error, look) =
        match match_camera_look(pipeline, &frame, &preview, &recipe.color.camera_profile) {
            Ok(look) => {
                tone_knots = Some(look.tone.tone.knots.clone());
                match &look.chroma {
                    Ok(fit) => {
                        "accepted".clone_into(&mut chroma_fit);
                        chroma_gains = Some(fit.chroma.gains);
                    }
                    Err(error) => chroma_fit = error.to_string(),
                }
                (
                    "accepted".to_owned(),
                    Some(look.tone.distribution_error),
                    Some(look),
                )
            }
            Err(error) => (error.to_string(), None, None),
        };

    let mut variants = vec![
        (
            "neutral",
            RenderingProfileSelection::NEUTRAL,
            RenderingAdjustments::NEUTRAL,
        ),
        (
            "standard",
            RenderingProfileSelection::STANDARD,
            RenderingAdjustments::default(),
        ),
    ];
    if let Some(look) = look {
        // Tone only, then tone plus chroma, to show what each stage buys.
        let tone_only = RenderingAdjustments {
            camera_tone: Some(look.tone.tone.clone()),
            ..RenderingAdjustments::default()
        };
        variants.push((
            "matched_tone",
            RenderingProfileSelection::MATCHED,
            tone_only,
        ));
        let mut full = RenderingAdjustments::default();
        look.store_in(&mut full);
        variants.push(("matched", RenderingProfileSelection::MATCHED, full));
    }
    // The static look Matched uses when no fit exists, whether or not this
    // photo's fit succeeded; it shows how much a fit buys over the defaults.
    variants.push((
        "fallback",
        RenderingProfileSelection::MATCHED,
        RenderingAdjustments::default(),
    ));
    let mut renderings = Vec::new();
    for (rendering, profile, adjustments) in variants {
        recipe.rendering = adjustments;
        recipe.rendering.profile = profile;
        let rendered =
            pipeline.render_preview_from_base(&base, &recipe, OutputPolicy::ClipToSrgb)?;
        let (proxy, camera_proxy) = Proxy::pair(&rendered.image, &camera)?;
        let difference = compare_proxies(&proxy, &camera_proxy)
            .context("proxies of the same pair must share a grid")?;
        renderings.push(RenderingLook {
            rendering,
            difference: difference.into(),
        });
    }

    Ok(SourceLook {
        file: file.file_name().map_or_else(
            || file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        camera: format!("{} {}", frame.info.clean_make, frame.info.clean_model),
        fit: fit_outcome,
        distribution_error,
        tone_knots,
        chroma_fit,
        chroma_gains,
        renderings,
    })
}

fn format_report(report: &LookReport) -> String {
    let mut lines = vec![
        "Errors are sRGB-encoded, 0..1 (multiply by 255 for 8-bit levels); bias < 0 means darker than the camera JPEG."
            .to_owned(),
    ];
    for source in &report.sources {
        lines.push(format!("\n{} ({})", source.file, source.camera));
        lines.push(format!("  tone fit: {}", source.fit));
        lines.push(match source.chroma_gains {
            Some(gains) => format!("  chroma fit: accepted, gains {gains:.2?}"),
            None => format!("  chroma fit: {}", source.chroma_fit),
        });
        lines.push(
            "  rendering     luma RMS   luma bias   mean error   p95 error   R/G/B RMS".to_owned(),
        );
        for look in &source.renderings {
            let difference = &look.difference;
            lines.push(format!(
                "  {:<12}  {:>8.4}   {:>+9.4}   {:>10.4}   {:>9.4}   {:.3}/{:.3}/{:.3}",
                look.rendering,
                difference.luminance_rms,
                difference.luminance_bias,
                difference.mean_color_error,
                difference.p95_color_error,
                difference.channel_rms[0],
                difference.channel_rms[1],
                difference.channel_rms[2],
            ));
        }
    }
    lines.join("\n")
}
