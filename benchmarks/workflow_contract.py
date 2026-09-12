"""Shared schema-v1 workflow gate and diagnostic phase definitions."""

from collections.abc import Iterable

COMMON_SAMPLE_GATES = frozenset(
    ("commands", "configuration", "behavior", "api", "diagnostics")
)
DIAGNOSTIC_PHASES = ("cold", "primed", "unchanged", "edited")


def required_sample_gates(
    arm: str,
    state: str,
    target_ids: Iterable[str],
    extraction_repeats: int,
) -> frozenset[str]:
    """Return every gate required to validate one persisted sample."""
    required = set(COMMON_SAMPLE_GATES)
    required.update(f"coverage:{target_id}" for target_id in target_ids)
    if arm != "native":
        required.add("extraction-set")
        required.update(
            f"repeat-set:{index}" for index in range(extraction_repeats)
        )
        if state == "edited":
            required.add("edited_ir")
    return frozenset(required)
