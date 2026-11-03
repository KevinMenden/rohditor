"""Resume-safe downloaders for the licensed raw datasets.

One module per source (`rawnind`, `pixelshift200`, `pixls`). Each turns the
upstream listing into `RemoteFile`s; `runner` downloads them and `http` does the
resumable, checksum-verified transfer. Run with
`uv run python -m rohditor_ml.data.download --help`.
"""
