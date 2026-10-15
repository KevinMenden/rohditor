//! Runs RawTherapee and darktable headlessly on a DNG and returns their
//! demosaiced output as linear RGB.
//!
//! Both tools are configured through the AppImage paths in
//! `ROHDITOR_RAWTHERAPEE` and `ROHDITOR_DARKTABLE`. AppImages are extracted
//! once into the output directory, because the CLI binaries live inside
//! them. Profile templates are in `testdata/reference/`.

use std::path::{Path, PathBuf};
use std::process::Command;

use rohditor_image::LinearRgbImage;

use crate::{ReferenceError, output_dir, tiff_read};

/// A demosaic algorithm both external tools implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedAlgorithm {
    Amaze,
    Rcd,
}

impl SharedAlgorithm {
    const fn rawtherapee_name(self) -> &'static str {
        match self {
            Self::Amaze => "amaze",
            Self::Rcd => "rcd",
        }
    }

    /// Little-endian `dt_iop_demosaic_method_t` value as hex.
    const fn darktable_method_hex(self) -> &'static str {
        match self {
            Self::Amaze => "01000000",
            Self::Rcd => "05000000",
        }
    }
}

/// An extracted AppImage directory (`squashfs-root`).
pub struct Tool {
    root: PathBuf,
}

impl Tool {
    /// Extract the AppImage named by environment variable `var`, or `None`
    /// when it is unset so callers can skip.
    pub fn from_env(var: &str, name: &str) -> Result<Option<Self>, ReferenceError> {
        let Some(appimage) = std::env::var_os(var) else {
            return Ok(None);
        };
        let dir = output_dir().join("tools").join(name);
        let root = dir.join("squashfs-root");
        if !root.exists() {
            std::fs::create_dir_all(&dir)?;
            let status = Command::new(&appimage)
                .arg("--appimage-extract")
                .current_dir(&dir)
                .output()?
                .status;
            if !status.success() {
                return Err(ReferenceError::Other(format!("could not extract {var}")));
            }
        }
        Ok(Some(Self { root }))
    }
}

fn template(name: &str) -> Result<String, ReferenceError> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/reference")
        .join(name);
    Ok(std::fs::read_to_string(path)?)
}

fn run(mut command: Command, what: &str) -> Result<(), ReferenceError> {
    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ReferenceError::Other(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// Demosaic `dng` with RawTherapee: neutral WB, no other processing, sRGB
/// output that is decoded back to linear. RawTherapee crops a few border
/// pixels, so the result can be smaller than the input; see
/// [`crate::truth::interior_window`].
pub fn run_rawtherapee(
    tool: &Tool,
    dng: &Path,
    algorithm: SharedAlgorithm,
    work_dir: &Path,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let pp3 = work_dir.join(format!("rt_{}.pp3", algorithm.rawtherapee_name()));
    std::fs::write(
        &pp3,
        template("rawtherapee.pp3")?.replace("@METHOD@", algorithm.rawtherapee_name()),
    )?;
    let out = work_dir.join(format!("rt_{}.tif", algorithm.rawtherapee_name()));
    let root = &tool.root;
    let mut command = Command::new(root.join("usr/bin/rawtherapee-cli"));
    // What the AppImage's GTK launch hook would export.
    command
        .env("APPDIR", root)
        .env("GTK_DATA_PREFIX", root)
        .env("GDK_BACKEND", "x11")
        .env(
            "GSETTINGS_SCHEMA_DIR",
            root.join("usr/share/glib-2.0/schemas"),
        )
        .env("GTK_EXE_PREFIX", root.join("usr"))
        .env("GTK_PATH", root.join("usr/lib/gtk-3.0"))
        .env(
            "GDK_PIXBUF_MODULE_FILE",
            root.join("usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"),
        )
        .env(
            "XDG_DATA_DIRS",
            format!("{}:/usr/share", root.join("usr/share").display()),
        )
        .args(["-q", "-Y", "-t", "-b16", "-p"])
        .arg(&pp3)
        .arg("-o")
        .arg(&out)
        .arg("-c")
        .arg(dng);
    run(command, "rawtherapee-cli")?;

    let mut image = tiff_read::read_rgb_tiff(&out)?;
    tiff_read::srgb_to_linear(&mut image);
    Ok(image)
}

/// Demosaic `dng` with darktable: only the demosaic history item is set, no
/// workflow presets, linear Rec.709 float output. Uses a throwaway config
/// and in-memory library so the user's darktable data is untouched.
pub fn run_darktable(
    tool: &Tool,
    dng: &Path,
    algorithm: SharedAlgorithm,
    work_dir: &Path,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let name = algorithm.rawtherapee_name();
    let xmp = work_dir.join(format!("dt_{name}.xmp"));
    std::fs::write(
        &xmp,
        template("darktable.xmp")?.replace("@METHOD@", algorithm.darktable_method_hex()),
    )?;
    let out = work_dir.join(format!("dt_{name}.tif"));
    // darktable refuses to overwrite unless told; remove a stale result.
    let _ = std::fs::remove_file(&out);
    let config = work_dir.join("dt-config");
    let cache = work_dir.join("dt-cache");
    let mut command = Command::new(tool.root.join("usr/bin/darktable-cli"));
    command
        .arg(dng)
        .arg(&xmp)
        .arg(&out)
        .args([
            "--apply-custom-presets",
            "false",
            "--icc-type",
            "LIN_REC709",
        ])
        .args(["--core", "--configdir"])
        .arg(&config)
        .arg("--cachedir")
        .arg(&cache)
        .args(["--library", ":memory:"])
        .args(["--conf", "plugins/darkroom/workflow=none"])
        .args(["--conf", "plugins/imageio/format/tiff/bpp=32"]);
    run(command, "darktable-cli")?;
    tiff_read::read_rgb_tiff(&out)
}
