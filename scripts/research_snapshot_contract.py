#!/usr/bin/env python3
"""Research snapshot contract helpers for promotion consumers."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def load_snapshot_execution_contract(path: str | None) -> dict[str, Any]:
    if not path:
        return {}
    manifest_path = Path(path)
    payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    surfaces = payload.get("source_surfaces") or []
    sampled_required_execution_surfaces: list[str] = []
    raw_full_fidelity_required_execution_surfaces: list[str] = []
    for surface in surfaces:
        if not isinstance(surface, dict):
            continue
        if surface.get("gate_category") != "required_for_execution":
            continue
        name = str(surface.get("name") or "<unknown>")
        if surface.get("raw_full_fidelity") is True:
            raw_full_fidelity_required_execution_surfaces.append(name)
        if surface.get("snapshot_sampled") is True:
            sampled_required_execution_surfaces.append(name)
    blocking_flags = [
        f"sampled_snapshot_required_for_execution_surface:{name}"
        for name in sorted(sampled_required_execution_surfaces)
    ]
    return {
        "manifest_path": str(manifest_path),
        "schema_version": payload.get("schema_version", ""),
        "snapshot_hash": payload.get("snapshot_hash", ""),
        "source_kind": payload.get("source_kind", ""),
        "sampled_required_execution_surfaces": sorted(sampled_required_execution_surfaces),
        "raw_full_fidelity_required_execution_surfaces": sorted(
            raw_full_fidelity_required_execution_surfaces
        ),
        "blocking_risk_flags": blocking_flags,
    }


def snapshot_blocks_execution_claim(snapshot_contract: dict[str, Any]) -> bool:
    return bool(snapshot_contract.get("blocking_risk_flags"))
