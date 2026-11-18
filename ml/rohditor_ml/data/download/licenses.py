"""License identifiers (SPDX) and the allow-list used when selecting files."""

CC0 = "CC0-1.0"
CC_BY = "CC-BY-4.0"
CC_BY_SA = "CC-BY-SA-4.0"
RESEARCH_ONLY = "LicenseRef-research-only"

# Files are only downloaded when their license is allowed. Non-commercial and
# research-only data stay out; widen with `--allow-license` for experiments only.
#
# CC BY-SA is allowed (decision for #116): it brings in most of RawNIND (its
# dataset-level license). The weights are not distributed for now; publishing
# them later would likely require releasing them under CC BY-SA too.
DEFAULT_ALLOWED = frozenset({CC0, CC_BY, CC_BY_SA})

# The licenses a *training/evaluation manifest* may contain. The build refuses
# anything else, and a test enforces it. Changing this is a licensing decision.
TRAINING_ALLOWED = DEFAULT_ALLOWED


def from_creative_commons_url(url: str) -> str:
    """Map a creativecommons.org license URL to an SPDX id.

    Unrecognised URLs become `LicenseRef-unknown:<url>`, which is never allowed
    by default, so an unexpected license can't slip into the download.
    """
    # Both "http" and "https" spellings occur in the wild.
    path = url.split("creativecommons.org/", 1)[-1].strip("/")
    parts = path.split("/")
    if parts[:2] == ["publicdomain", "zero"]:
        return CC0
    if len(parts) >= 3 and parts[0] == "licenses":
        return f"CC-{parts[1].upper()}-{parts[2]}"
    return f"LicenseRef-unknown:{url}"
