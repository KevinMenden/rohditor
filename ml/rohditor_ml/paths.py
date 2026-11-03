"""Well-known locations inside the `ml/` folder."""

from pathlib import Path

ML_ROOT = Path(__file__).resolve().parents[1]
# Downloaded source data. Git-ignored; never committed or uploaded.
RAW_DIR = ML_ROOT / "data" / "raw"

# The private camera corpus is validation-only: no code in this package may read it
# while building training data.
PRIVATE_DIR = ML_ROOT.parent / "testdata" / "private"


def assert_not_private(path: Path) -> None:
    resolved = Path(path).resolve()
    if resolved == PRIVATE_DIR or PRIVATE_DIR in resolved.parents:
        raise PermissionError(f"{path} is in the private corpus, which must never enter training data")
