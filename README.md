# Rohditor

Rohditor is a Linux-first Rust RAW photo editor.
It provides a deterministic CPU development pipeline, optional GPU previews,
and a desktop editor plus headless CLI.

This is an ongoing project with many features still missing.

## Build and test

```console
cargo build --workspace
./scripts/check.sh
cargo bench -p rohditor-core --bench pipeline_stages
```

The private camera corpus is intentionally not checked in. Opt-in validation:

```console
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

## Run

```console
cargo run --release -p rohditor-desktop
cargo run -p rohditor-cli -- inspect path/to/file.ARW
cargo run --release -p rohditor-cli -- develop input.ARW output.png
```

The desktop supports `--processor auto|gpu|cpu`, `--renderer auto|wgpu|glow`,
and an optional input path. The CLI also supports embedded-preview extraction,
quality-crop generation, and LibRaw verification.

Project guidelines live in [AGENTS.md](AGENTS.md); the implementation checklist is below.

## What happens when a RAW is opened

Opening a file runs on the desktop's background worker thread
(`apps/desktop/src/coordinator/worker.rs`). The UI shows the embedded JPEG first
and replaces it once the developed preview arrives. The steps up to `decode`
run one after another.

```mermaid
flowchart TD
    A["Open file<br/>RawlerDecoder::open: detect format"] --> B["probe<br/>metadata only, no pixel decode"]
    B --> C["Optics match<br/>Lensfun profile lookup"]
    C --> D["Embedded JPEG placeholder<br/>extract + decode + orient<br/>shown immediately"]
    D --> E["decode<br/>full sensor mosaic, u16, ~24 MP<br/>(rawler, Sony ARW decompression)"]
    E -->|RawReady| F["queue_preview<br/>newest request wins, older ones cancelled"]

    F --> G{"Preview cache hit?<br/>decoded / reconstructed /<br/>demosaiced / adjusted"}
    G -->|"adjusted hit"| Z["Copy cached image + histogram"]
    G -->|miss| H{"GPU sensor path<br/>available?"}

    subgraph SENSOR["Sensor development, always at full sensor resolution"]
        direction TB
        S1["Normalize<br/>crop, black/white level to f32 mosaic"] --> S2["Highlight handling<br/>Clip by default; LocalRatios / Opposed optional"]
        S2 --> S3["Demosaic<br/>MHC default; RCD / AMaZE optional<br/>mosaic to full-size linear RGB"]
        S3 --> S4["Capture sharpening<br/>opt-in, masked Richardson-Lucy"]
    end

    H -->|"yes: wgpu"| GU["Upload mosaic to GPU<br/>run the same four stages in shaders"]
    GU --> SENSOR
    H -->|"no: CPU"| SENSOR

    SENSOR --> O["Optics correction<br/>distortion / vignetting / TCA, if a profile matched"]
    O --> R["Area-reduce to 2560 px long edge<br/>full-size image to preview size"]
    R --> CC["White balance + camera color matrix<br/>to Rec.2020 linear<br/>cached as the DemosaicedBase"]
    CC --> ADJ["Edit adjustments<br/>light, color, HSL, grading, tone curve"]
    ADJ --> OUT["Output conversion<br/>Rec.2020 to sRGB 8-bit<br/>+ histogram"]
    OUT --> UI["PreviewReady<br/>texture upload in the UI"]
    Z --> UI
```

Everything left of "Area-reduce" runs on the full ~24 MP sensor, not on the
2560 px preview, so a first open pays for normalize, highlight, demosaic, and
optics at full size. Once the base is cached, slider edits only re-run the
steps from "Edit adjustments" onwards on the small image.

The cache is layered (`apps/desktop/src/preview_cache/`) so an edit re-runs only
what its recipe fields invalidate: changing exposure keeps the demosaiced base;
changing the highlight method or demosaic algorithm rebuilds from the decoded
mosaic.

## License

Rohditor is licensed under the GNU General Public License, version 3 or (at your option) any later version. See [LICENSE](LICENSE).
