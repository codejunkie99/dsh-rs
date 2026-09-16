#!/usr/bin/env python3
"""Refresh the dsh-rs progress board after a loop cycle.

The loop runner calls this only when the project provides the script. The
runner may emit one machine-readable line:

    PROGRESS_JSON: {"skills": [35, 40], "agent": [40, 45]}

Only known ids and validated 0..100 ranges are applied. Without that line the
board still records the cycle and evidence while preserving the last estimate.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from pathlib import Path
from typing import Any


PROGRESS_LINE = re.compile(r"^PROGRESS_JSON:\s*(\{.*\})\s*$", re.MULTILINE)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def range_value(value: Any) -> list[int] | None:
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        low = high = int(value)
    elif isinstance(value, list) and len(value) == 2:
        if not all(isinstance(item, (int, float)) and not isinstance(item, bool) for item in value):
            return None
        low, high = (int(value[0]), int(value[1]))
    else:
        return None
    if not (0 <= low <= high <= 100):
        return None
    return [low, high]


def parse_progress(run_output: Path | None, known_ids: set[str]) -> tuple[dict[str, list[int]], str | None]:
    if run_output is None or not run_output.exists():
        return {}, None
    text = run_output.read_text(encoding="utf-8", errors="replace")
    matches = PROGRESS_LINE.findall(text)
    if not matches:
        return {}, None
    try:
        payload = json.loads(matches[-1])
    except json.JSONDecodeError as error:
        return {}, f"invalid PROGRESS_JSON: {error.msg}"
    if not isinstance(payload, dict):
        return {}, "PROGRESS_JSON must be an object"
    updates: dict[str, list[int]] = {}
    for key, value in payload.items():
        if key not in known_ids:
            continue
        parsed = range_value(value)
        if parsed is None:
            return {}, f"invalid remaining range for {key!r}"
        updates[key] = parsed
    return updates, None


def summary_from(run_output: Path | None) -> str:
    if run_output is None or not run_output.exists():
        return "No runner output recorded."
    lines = run_output.read_text(encoding="utf-8", errors="replace").splitlines()
    for line in lines:
        stripped = line.strip()
        if stripped.startswith(("DONE:", "Status:", "Implemented", "Shipped")):
            return stripped[:280]
    for line in lines:
        stripped = line.strip()
        if stripped:
            return stripped[:280]
    return "Runner produced no summary."


def format_range(value: list[int]) -> str:
    return f"{value[0]}%" if value[0] == value[1] else f"{value[0]}–{value[1]}%"


def render_markdown(state: dict[str, Any]) -> str:
    loop = state.get("loop", {})
    updated = state.get("updated_at") or "not yet updated"
    lines = [
        "# DeepSeek Harness Rust parity — progress board",
        "",
        "> Remaining percentages are conservative engineering estimates, not completion proofs. This file is refreshed by `scripts/update-progress-board.py` after each loop cycle.",
        "",
        f"**Last update:** `{updated}`",
        f"**Loop:** cycle `{loop.get('cycle', '?')}`, status `{loop.get('status', 'unknown')}`",
        f"**Latest slice:** {loop.get('last_summary', 'No cycle recorded.')}",
        "",
        "## Workstreams",
        "",
        "| Workstream | Remaining | Baseline | Confidence | Status |",
        "|---|---:|---:|---|---|",
    ]
    for stream in state.get("workstreams", []):
        lines.append(
            f"| {stream['label']} | **{format_range(stream['remaining'])}** | {format_range(stream['baseline_remaining'])} | {stream.get('confidence', 'unknown')} | {stream.get('status', 'unknown')} |"
        )

    lines.extend(
        [
            "",
            "## Milestones",
            "",
            "| Milestone | Remaining | Baseline | Confidence | Status |",
            "|---|---:|---:|---|---|",
        ]
    )
    for milestone in state.get("milestones", []):
        lines.append(
            f"| {milestone['label']} | **{format_range(milestone['remaining'])}** | {format_range(milestone['baseline_remaining'])} | {milestone.get('confidence', 'unknown')} | {milestone.get('status', 'unknown')} |"
        )

    for stream in state.get("workstreams", []):
        lines.extend(["", f"### {stream['label']}", ""])
        shipped = stream.get("shipped", [])
        upcoming = stream.get("next", [])
        if shipped:
            lines.append("Shipped:")
            lines.extend(f"- {item}" for item in shipped)
        if upcoming:
            lines.append("Next:")
            lines.extend(f"- {item}" for item in upcoming)
        evidence = stream.get("evidence", [])
        if evidence:
            lines.append(f"Evidence: {', '.join(f'`{item}`' for item in evidence)}")

    lines.extend(
        [
            "",
            "## Loop activity",
            "",
            "| Cycle | Status | Summary | Recorded |",
            "|---:|---|---|---|",
        ]
    )
    activity = state.get("activity", [])
    if not activity:
        lines.append("| — | — | No cycles recorded yet. | — |")
    else:
        for item in reversed(activity[-12:]):
            summary = item.get("summary", "").replace("|", "\\|")
            lines.append(
                f"| {item.get('cycle', '—')} | {item.get('status', '—')} | {summary} | {item.get('recorded_at', '—')} |"
            )

    lines.extend(
        [
            "",
            "## Update contract",
            "",
            "A loop runner can update estimates by emitting one line such as `PROGRESS_JSON: {\"skills\": [35, 40], \"agent\": [40, 45]}`. Unknown keys are ignored; invalid ranges are recorded as an error and do not overwrite the last estimate.",
            "",
        ]
    )
    return "\n".join(lines)


def atomic_write(path: Path, content: str) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", default=".", type=Path)
    parser.add_argument("--cycle", type=int, default=0)
    parser.add_argument("--status", default="manual")
    parser.add_argument("--run-output", type=Path)
    parser.add_argument("--log", type=Path)
    args = parser.parse_args()

    workdir = args.workdir.resolve()
    state_path = workdir / "progress-board.json"
    markdown_path = workdir / "PROGRESS.md"
    if not state_path.exists():
        print(f"missing {state_path}; create the board state first", file=sys.stderr)
        return 2
    state = json.loads(state_path.read_text(encoding="utf-8"))
    known_ids = {
        item["id"] for item in state.get("workstreams", []) + state.get("milestones", [])
    }
    updates, parse_error = parse_progress(args.run_output, known_ids)
    summary = summary_from(args.run_output)

    for collection in (state.get("workstreams", []), state.get("milestones", [])):
        for item in collection:
            if item["id"] in updates:
                item["remaining"] = updates[item["id"]]
                item["estimate_source"] = "runner PROGRESS_JSON"

    timestamp = utc_now()
    state["updated_at"] = timestamp
    state.setdefault("loop", {})
    state["loop"].update(
        {
            "cycle": args.cycle,
            "status": args.status,
            "last_summary": summary,
            "last_run": str(args.run_output.resolve()) if args.run_output else None,
            "last_log": str(args.log.resolve()) if args.log else None,
            "estimate_parse_error": parse_error,
        }
    )
    activity = state.setdefault("activity", [])
    activity.append(
        {
            "cycle": args.cycle,
            "status": args.status,
            "summary": summary,
            "recorded_at": timestamp,
        }
    )
    state["activity"] = activity[-12:]

    atomic_write(state_path, json.dumps(state, indent=2, ensure_ascii=False) + "\n")
    atomic_write(markdown_path, render_markdown(state))
    print(f"progress board updated: {markdown_path}")
    if parse_error:
        print(f"progress estimate not changed: {parse_error}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
