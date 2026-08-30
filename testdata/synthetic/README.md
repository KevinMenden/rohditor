# Synthetic fixtures

The CPU reference tests currently construct their small, redistributable sensor
mosaics directly in Rust so each sample and expected value remain adjacent.
File-backed synthetic RAW fixtures can be added here when the decoder boundary
needs them. Private camera files belong in the ignored sibling directory
`testdata/private/`.
