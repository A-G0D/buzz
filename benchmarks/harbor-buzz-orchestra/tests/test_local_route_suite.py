"""Checks for the deterministic synthetic local route task suite."""

import pytest

from harbor_buzz_orchestra.local_route_suite import (
    LOCAL_ROUTE_TASKS,
    grade_arithmetic,
    grade_constraint_aggregation,
    grade_exact_json,
    grade_kebab_case,
    grade_local_route_task,
    grade_long_context_lookup,
    grade_malformed_json_repair,
    grade_records_to_json,
    grade_rust_function,
    grade_sorted_words,
    grade_table_to_tsv,
)


def test_suite_has_ten_fixed_nonempty_prompts():
    assert [task.task_id for task in LOCAL_ROUTE_TASKS] == [
        "arithmetic",
        "exact_json",
        "rust_function",
        "sorted_words",
        "kebab_case",
        "records_to_json",
        "table_to_tsv",
        "constraint_aggregation",
        "long_context_lookup",
        "malformed_json_repair",
    ]
    assert all(task.prompt and task.prompt == task.prompt.strip() for task in LOCAL_ROUTE_TASKS)
    long_context = next(task for task in LOCAL_ROUTE_TASKS if task.task_id == "long_context_lookup")
    assert len(long_context.prompt) > 700


@pytest.mark.parametrize("output", ["323", " 323\n"])
def test_arithmetic_grade(output):
    assert grade_arithmetic(output)


@pytest.mark.parametrize("output", ["324", "The answer is 323", "323.0"])
def test_arithmetic_rejects_nonexact_answers(output):
    assert not grade_arithmetic(output)


def test_exact_json_grade():
    assert grade_exact_json('{"status":"ready","count":3}')
    assert grade_exact_json('{ "count": 3, "status": "ready" }')


@pytest.mark.parametrize(
    "output",
    [
        '{"status":"ready","count":true}',
        '{"status":"ready","count":3,"extra":0}',
        '{"status":"ready","count":2,"count":3}',
        '{"status":"ready","count":3} trailing',
        'Here is the JSON: {"status":"ready","count":3}',
        "not JSON",
    ],
)
def test_exact_json_rejects_malformed_or_extra_data(output):
    assert not grade_exact_json(output)


@pytest.mark.parametrize(
    "output",
    [
        "fn add(a: i32, b: i32) -> i32 { a + b }",
        "pub fn add ( a:i32,b : i32 ) -> i32 { return a + b; }",
        "```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```",
    ],
)
def test_rust_function_grade(output):
    assert grade_rust_function(output)


@pytest.mark.parametrize(
    "output",
    [
        "fn add(a: i64, b: i64) -> i64 { a + b }",
        "fn add(a: i32, b: i32) -> i32 { a - b }",
        "Here is the function: fn add(a: i32, b: i32) -> i32 { a + b }",
        "```rust\nfn add(a: i32, b: i32) -> i32 { a + b }",
    ],
)
def test_rust_function_rejects_wrong_or_extra_content(output):
    assert not grade_rust_function(output)


def test_sorted_words_grade_and_rejects_wrong_order_or_commentary():
    assert grade_sorted_words("alder\nbirch\ncedar\nmaple")
    assert not grade_sorted_words("alder\ncedar\nbirch\nmaple")
    assert not grade_sorted_words("Sorted words:\nalder\nbirch\ncedar\nmaple")


def test_kebab_case_grade_is_exact():
    assert grade_kebab_case("blue-otter-relay\n")
    assert not grade_kebab_case("Blue-Otter-Relay")
    assert not grade_kebab_case("blue_otter_relay")


def test_record_transformation_checks_order_types_and_extra_fields():
    assert grade_records_to_json(
        '[{"sku":"A12","quantity":2},{"quantity":5,"sku":"B07"}]'
    )
    assert not grade_records_to_json(
        '[{"sku":"A12","quantity":2},{"sku":"B07","quantity":true}]'
    )
    assert not grade_records_to_json(
        '[{"sku":"A12","quantity":2,"extra":0},{"sku":"B07","quantity":5}]'
    )
    assert not grade_records_to_json(
        '[{"sku":"B07","quantity":5},{"sku":"A12","quantity":2}]'
    )


def test_table_to_tsv_grade_is_exact():
    assert grade_table_to_tsv("name\tscore\nAda\t91\nLin\t87\n")
    assert not grade_table_to_tsv("| name | score |\n|---|---|\n|Ada|91|")


def test_constraint_aggregation_grade_and_rejects_wrong_ties_or_format():
    assert grade_constraint_aggregation("red\t22\nblue\t12\ngreen\t12\nyellow\t12")
    assert not grade_constraint_aggregation(
        "red\t22\ngreen\t12\nblue\t12\nyellow\t12"
    )
    assert not grade_constraint_aggregation(
        "red: 22\nblue: 12\ngreen: 12\nyellow: 12"
    )


def test_long_context_lookup_grade_is_exact():
    assert grade_long_context_lookup("A-088 | Lio\n")
    assert not grade_long_context_lookup("A-042 | Rafi")
    assert not grade_long_context_lookup("A-088 | Rafi")
    assert not grade_long_context_lookup("The answer is A-088 | Lio")


def test_malformed_json_repair_grade_is_exact():
    assert grade_malformed_json_repair(
        '{"id":"A-17","flags":[true,false],"meta":{"owner":"Nia"}}'
    )
    assert not grade_malformed_json_repair(
        '{"id":"A-17","flags":[true,false,],"meta":{"owner":"Nia"}}'
    )
    assert not grade_malformed_json_repair(
        '```json\n{"id":"A-17","flags":[true,false],"meta":{"owner":"Nia"}}\n```'
    )


def test_dispatch_rejects_unknown_task_id():
    with pytest.raises(ValueError, match="unknown local route task"):
        grade_local_route_task("unknown", "anything")
