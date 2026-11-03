"""Well-known locations inside the `ml/` folder."""

from pathlib import Path

ML_ROOT = Path(__file__).resolve().parents[1]
# Downloaded source data. Git-ignored; never committed or uploaded.
RAW_DIR = ML_ROOT / "data" / "raw"
