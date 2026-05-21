"""Shared AutoFactor runtime-contract helpers.

The promotion path should prefer typed registry contracts over name-only
runtime mapping guesses. Name inference remains as a compatibility fallback for
older artifacts that do not yet include ``factor-registry-preview.json``.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

ContractIndex = dict[tuple[str, str], dict[str, Any]]


def load_factor_registry_runtime_contracts(paths: list[str]) -> ContractIndex:
    contracts: ContractIndex = {}
    for raw_path in paths:
        if not raw_path:
            continue
        path = Path(raw_path)
        payload = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(payload, dict):
            rows = payload.get("entries") or payload.get("factors") or payload.get("rows") or []
        elif isinstance(payload, list):
            rows = payload
        else:
            raise ValueError(f"{path} must decode to a JSON object or array")

        if not isinstance(rows, list):
            raise ValueError(f"{path} registry rows must be an array")

        for row in rows:
            if not isinstance(row, dict):
                continue
            name = str(row.get("factor_name") or row.get("name") or "")
            target = str(row.get("target") or "")
            contract = row.get("runtime_contract")
            if not name or not isinstance(contract, dict):
                continue
            contracts[(name, target)] = {
                "factor_name": name,
                "target": target,
                "dsl_hash": str(row.get("dsl_hash") or ""),
                "ast_json": row.get("ast_json"),
                "runtime_contract": contract,
                "source_path": str(path),
            }
    return contracts


def runtime_contract_for_row(
    contracts: ContractIndex,
    *,
    factor_name: str,
    target: str,
) -> dict[str, Any] | None:
    return contracts.get((factor_name, target)) or contracts.get((factor_name, ""))


def mapping_from_runtime_contract(
    record: dict[str, Any] | None,
    *,
    factor_name: str,
    target: str,
    require_runtime_contract: bool,
) -> tuple[dict[str, str] | None, dict[str, Any] | None, list[str]]:
    if record is None:
        if require_runtime_contract:
            return (
                None,
                None,
                [f"runtime_contract_missing:{factor_name}:{target or '<missing-target>'}"],
            )
        return None, None, []

    contract = record.get("runtime_contract") or {}
    if not isinstance(contract, dict):
        return None, contract, ["runtime_contract_invalid:not_object"]

    blockers = [str(item) for item in contract.get("blockers") or []]
    status = str(contract.get("status") or "blocked")
    if status != "supported":
        if not blockers:
            blockers.append(f"runtime_contract_not_supported:{status}")
        return None, contract, blockers

    runtime_score = str(contract.get("runtime_score") or "")
    strategy_profile = str(contract.get("strategy_profile") or "")
    if not runtime_score:
        blockers.append("runtime_contract_missing_runtime_score")
    if not strategy_profile:
        blockers.append("runtime_contract_missing_strategy_profile")
    if blockers:
        return None, contract, blockers

    mapping = {
        "strategy_profile": strategy_profile,
        "strategy_family": str(contract.get("strategy_family") or strategy_profile),
        "runtime_score": runtime_score,
        "runtime_contract_status": status,
        "runtime_contract_source": str(
            contract.get("mapping_source") or record.get("source_path") or "factor_registry_preview"
        ),
        "dsl_hash": str(record.get("dsl_hash") or ""),
    }
    return mapping, contract, []
