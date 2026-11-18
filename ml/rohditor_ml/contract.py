"""The shared input/output contract (`ml/contract.json`), loaded once.

The JSON file is the source of truth that the training and Rust-wiring issues
read; the constants here are what the Python code needs from it.
"""

import json

from .paths import ML_ROOT

CONTRACT = json.loads((ML_ROOT / "contract.json").read_text(encoding="utf-8"))

PATCH_SIZE: int = CONTRACT["patches"]["size"]
SIZE_MULTIPLE: int = CONTRACT["input"]["size_multiple"]
WB_RED_RANGE: tuple[float, float] = tuple(CONTRACT["white_balance"]["training_range"]["red"])
WB_BLUE_RANGE: tuple[float, float] = tuple(CONTRACT["white_balance"]["training_range"]["blue"])
