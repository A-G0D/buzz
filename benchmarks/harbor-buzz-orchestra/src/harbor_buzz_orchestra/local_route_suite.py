"""Fixed, non-private prompts and graders for a local route smoke check."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class LocalRouteTask:
    """A deterministic synthetic prompt used by local model checks."""

    task_id: str
    prompt: str


LOCAL_ROUTE_TASKS = (
    LocalRouteTask(
        task_id="arithmetic",
        prompt="What is 17 multiplied by 19? Reply with only the integer.",
    ),
    LocalRouteTask(
        task_id="exact_json",
        prompt=(
            "Return exactly this JSON object and no other text: "
            '{"status":"ready","count":3}'
        ),
    ),
    LocalRouteTask(
        task_id="rust_function",
        prompt=(
            "Write a Rust function named add that takes a and b as i32 and "
            "returns their sum as i32. Return only the function."
        ),
    ),
    LocalRouteTask(
        task_id="sorted_words",
        prompt=(
            "Sort these words alphabetically and return one word per line, "
            "with no numbering or commentary: cedar, alder, maple, birch."
        ),
    ),
    LocalRouteTask(
        task_id="kebab_case",
        prompt=(
            "Convert this phrase to lowercase kebab-case. Return only the "
            "result: Blue Otter Relay."
        ),
    ),
    LocalRouteTask(
        task_id="records_to_json",
        prompt=(
            "Convert these records to a JSON array. Use exactly the keys "
            "sku (string) and quantity (integer), preserve row order, and "
            "return no other text: sku=A12, quantity=2; "
            "sku=B07, quantity=5."
        ),
    ),
    LocalRouteTask(
        task_id="table_to_tsv",
        prompt=(
            "Convert this table to TSV. Preserve its header and row order. "
            "Return the literal text for the three rows, with one tab "
            "character between fields. Do not provide code, a Markdown "
            "table, fences, or explanation: "
            "header=name|score; row=Ada|91; row=Lin|87."
        ),
    ),
    LocalRouteTask(
        task_id="constraint_aggregation",
        prompt=(
            "Use only the records below. Keep records whose status is exactly "
            "active and whose quantity is at least 3. For each kept record, "
            "calculate quantity multiplied by unit_cost. Sum those amounts "
            "by category. Include categories whose sum is at least 12. Output "
            "one line per category as category, one literal tab character, "
            "then sum. Order by sum descending, then category ascending. "
            "Use no header or explanation.\n\n"
            "Records (reference | category | status | quantity | unit_cost):\n"
            "R-01 | red | active | 4 | 3\n"
            "R-02 | red | inactive | 10 | 2\n"
            "R-03 | blue | active | 2 | 5\n"
            "R-04 | blue | active | 3 | 4\n"
            "R-05 | red | active | 5 | 2\n"
            "R-06 | green | active | 3 | 4\n"
            "R-07 | yellow | active | 4 | 3\n"
            "R-08 | green | inactive | 100 | 1\n"
            "R-09 | purple | active | 2 | 100"
        ),
    ),
    LocalRouteTask(
        task_id="long_context_lookup",
        prompt=(
            "The following is a complete synthetic project log. Each row is "
            "reference | team | status | owner | due day.\n\n"
            "A-014 | Birch | ready | Elin | Monday\n"
            "A-027 | Atlas | blocked | Noah | Tuesday\n"
            "A-031 | Cove | waiting | Mira | Wednesday\n"
            "A-042 | Atlas | waiting | Rafi | Thursday\n"
            "A-055 | Atlas | ready | Rafi | Friday\n"
            "A-061 | Elm | waiting | Rafi | Thursday\n"
            "A-073 | Cove | blocked | Inez | Monday\n"
            "A-088 | Atlas | waiting | Lio | Friday\n"
            "A-094 | Birch | waiting | Rafi | Thursday\n"
            "B-103 | Atlas | blocked | Rafi | Wednesday\n"
            "B-117 | Delta | waiting | Sol | Thursday\n"
            "B-129 | Cove | ready | Rafi | Tuesday\n"
            "B-144 | Elm | blocked | Rafi | Thursday\n"
            "B-158 | Atlas | ready | Mira | Monday\n"
            "B-171 | Birch | blocked | Sol | Tuesday\n"
            "B-183 | Delta | ready | Lio | Wednesday\n"
            "C-201 | Birch | ready | Noah | Friday\n"
            "C-219 | Atlas | waiting | Mira | Tuesday\n"
            "C-227 | Cove | waiting | Rafi | Saturday\n"
            "C-240 | Elm | ready | Inez | Monday\n"
            "C-266 | Delta | blocked | Noah | Friday\n"
            "D-301 | Atlas | waiting | Elin | Sunday\n"
            "D-315 | Birch | waiting | Lio | Tuesday\n"
            "D-332 | Cove | ready | Rafi | Thursday\n\n"
            "Find the row for team Atlas with status waiting and due day Friday. "
            "Return its reference and owner as reference | owner. Output no explanation."
        ),
    ),
    LocalRouteTask(
        task_id="malformed_json_repair",
        prompt=(
            "Repair the input using only these rules: (1) convert single-quoted "
            "strings to JSON double-quoted strings; (2) quote bare object keys; "
            "(3) remove trailing commas before a closing brace or bracket; "
            "(4) preserve all values and their order; do not add or infer fields. "
            "Output compact JSON only, with no spaces outside strings and no "
            "explanation.\n\nInput:\n"
            "{id:'A-17', flags:[true,false,], meta:{owner:'Nia',},}"
        ),
    ),
)

_RUST_ADD_FUNCTION = re.compile(
    r"\A\s*(?:pub\s+)?fn\s+add\s*\(\s*a\s*:\s*i32\s*,\s*"
    r"b\s*:\s*i32\s*\)\s*->\s*i32\s*\{\s*"
    r"(?:a\s*\+\s*b\s*;?|return\s+a\s*\+\s*b\s*;)\s*\}\s*\Z",
    re.DOTALL,
)


def grade_arithmetic(output: str) -> bool:
    """Accept the exact integer answer, with optional surrounding whitespace."""
    return output.strip() == "323"


def grade_exact_json(output: str) -> bool:
    """Accept the requested JSON object and reject extra or wrongly typed data."""
    try:
        value = json.loads(output, object_pairs_hook=_unique_object)
    except (TypeError, ValueError):
        return False
    return (
        isinstance(value, dict)
        and set(value) == {"status", "count"}
        and value["status"] == "ready"
        and type(value["count"]) is int
        and value["count"] == 3
    )


def _unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    """Build a JSON object while rejecting duplicate keys."""
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON object key")
        value[key] = item
    return value


def grade_rust_function(output: str) -> bool:
    """Check the requested Rust function, allowing one Markdown code fence."""
    source = output.strip()
    lines = source.splitlines()
    if (
        len(lines) >= 3
        and lines[0].strip().startswith("```")
        and lines[-1].strip() == "```"
    ):
        source = "\n".join(lines[1:-1]).strip()
    return _RUST_ADD_FUNCTION.fullmatch(source) is not None


def grade_sorted_words(output: str) -> bool:
    """Accept only the requested sorted words, one per line."""
    return output.strip() == "alder\nbirch\ncedar\nmaple"


def grade_kebab_case(output: str) -> bool:
    """Accept only the exact lowercase kebab-case transformation."""
    return output.strip() == "blue-otter-relay"


def grade_records_to_json(output: str) -> bool:
    """Accept the exact two records and reject duplicate or extra JSON data."""
    try:
        value = json.loads(output, object_pairs_hook=_unique_object)
    except (TypeError, ValueError):
        return False
    expected = [
        {"sku": "A12", "quantity": 2},
        {"sku": "B07", "quantity": 5},
    ]
    return (
        type(value) is list
        and all(type(record) is dict for record in value)
        and value == expected
    )


def grade_table_to_tsv(output: str) -> bool:
    """Accept only the exact header and rows in tab-separated form."""
    return output.strip() == "name\tscore\nAda\t91\nLin\t87"


def grade_constraint_aggregation(output: str) -> bool:
    """Grade the exact multi-step filter, aggregate, threshold, and order task."""
    return output.strip() == "red\t22\nblue\t12\ngreen\t12\nyellow\t12"


def grade_long_context_lookup(output: str) -> bool:
    """Accept only the unique reference/owner pair in the fixed project log."""
    return output.strip() == "A-088 | Lio"


def grade_malformed_json_repair(output: str) -> bool:
    """Accept only the exact repaired JSON object, without extra formatting."""
    return output.strip() == '{"id":"A-17","flags":[true,false],"meta":{"owner":"Nia"}}'


def grade_local_route_task(task_id: str, output: str) -> bool:
    """Grade one known fixed task; reject unknown task IDs explicitly."""
    graders = {
        "arithmetic": grade_arithmetic,
        "exact_json": grade_exact_json,
        "rust_function": grade_rust_function,
        "sorted_words": grade_sorted_words,
        "kebab_case": grade_kebab_case,
        "records_to_json": grade_records_to_json,
        "table_to_tsv": grade_table_to_tsv,
        "constraint_aggregation": grade_constraint_aggregation,
        "long_context_lookup": grade_long_context_lookup,
        "malformed_json_repair": grade_malformed_json_repair,
    }
    try:
        grader = graders[task_id]
    except KeyError as error:
        raise ValueError(f"unknown local route task: {task_id}") from error
    return grader(output)
