# Skill use fixture v1

This is a small, synthetic fixture for evaluating skill selection and use. It
does not run models, import skills, call a provider, or contain evaluation
results. The fixture alone is **not evidence** that skills improve performance
or that any skill is useful/useless.

## Paired protocol

Run each case in a fresh, isolated session in randomized condition order. Keep
the model revision, base prompt, tool set, generation settings, task text, and
synthetic evidence fixed across conditions. Collect the required structured
route decision and task answer in each run.

| Condition | Extra skill bodies shown |
| --- | --- |
| `no_skill` | None; catalog descriptions remain visible for route decisions |
| `focused_skill` | Exactly the case's one pre-registered candidate; on `none` cases it is a plausible distractor |
| `small_pack` | The case's three candidate bodies, including distractors/overlap where listed |

This is a context-injection comparison. It does **not** prove filesystem
ingestion, nested `AGENTS.md` discovery, dynamic tool loading, or safe install
behavior; those need separate runtime tests. The candidate bodies are synthetic
and pinned in `dataset.json`.

## Required per-run record

Record the condition and case IDs, model/revision and full prompt/config hashes,
verifier version, task outcome, route action, `selected_skill_ids` (skills the
agent chooses or requests to apply), `review_subject_ids` (catalog/staged
records examined), `target_skill_ids` (records proposed for change),
`ingest_decision`, observed `opened_skill_ids`, and observed
`performed_import_ids`. Observe opened/imported IDs from runtime traces or
isolated filesystem state, not only the agent's self-report. Use `[]` when
measurement confirms none and `null` when instrumentation is unavailable. A
selected skill may be absent in the assigned condition; record actually opened
IDs separately. Also record skill bytes/tokens added, latency, and cost. Capture
counts from the runtime; do not estimate missing values. Do not place
credentials or private project content in this fixture or its receipts.

## Report these dimensions separately

- **Task success:** case-specific acceptance fields/signals, reported per case
  and condition; do not substitute rationale quality for a correct answer.
- **Regression:** paired pass-to-fail and fail-to-pass transitions, plus
  unchanged pairs. Show the cases; do not hide them in an overall average.
- **Skill overhead:** measured added context, skill reads, tool calls, latency,
  and cost versus that case's `no_skill` run. Unavailable measurements stay
  null.
- **Selection/abstention:** exact `route_action` and both ID fields against `route_gold`;
  separately count unnecessary activation, missed useful skill, wrong skill,
  unsupported create/merge/split proposals, and correct abstention.
- **Ingestion discipline:** compare the `ingest_decision` with its gold value,
  then separately record `performed_import_ids`. A true
  gold decision occurs only in the locally reviewed, explicitly approved case;
  no fixture condition itself performs a download or mutation.

Do not invent scores from this static fixture. Before using a model, pre-register
the verifier and thresholds, use paired repeats, retain raw per-case results,
and report the exact condition. Do not claim semantic usefulness from name or
keyword overlap.

## Scope and limits

The eight cases are answerable from their supplied synthetic task/evidence.
Gold route actions are explicit hypotheses for this fixture, not universal
policy truths. The overlap and split/create cases intentionally include workload
evidence because one task or matching words alone cannot establish reuse,
redundancy, or a durable need for a new skill. Human review should adjudicate
ambiguous proposals before using them to revise the catalog.

Validate the data-only fixture and its case-by-condition pairing with Python's
standard library (from this directory):

```bash
python3 validate_dataset.py
python3 -m unittest -v test_validate_dataset.py
```

The validator checks fixture structure, IDs, skill references, route labels,
and the documented three-condition matrix. Its `planned cells` count is not
run data or an evaluation score; it never contacts a model or provider.
It validates static fixture consistency only; it does not establish runtime
authorization, filesystem containment, or safe ingestion behavior.

## Static evaluation receipts

`report_schema.json` and `validate_report.py` define a separate receipt contract
for a future run. The validator binds a report to the pinned fixture digest and
requires exactly the 24 planned case × condition cells. It checks each cell's
config digest against the report, and can compare the full config identity with
an externally preregistered JSON object:

```bash
python3 validate_report.py report.json --expected-config preregistered-config.json
python3 -m unittest -v test_validate_report.py
```

The config object records prompt, toolset, generation-settings, route-profile,
and verifier identities. Model/provider identity has separate self-reported
and observed records. Telemetry uses a value plus local evidence reference, or
`null` plus an unavailable reason; it never fills gaps with estimates. Unknown
fields (including score fields), unknown fixture IDs, duplicate/missing cells,
and mismatched fixture or config identities fail validation. Evidence paths are
checked for safe relative form but are not opened or verified.

This validates receipt structure and declared provenance only. It does not
authenticate evidence, check answer/verdict correctness, calculate scores, or
establish skill efficacy. No model run or provider call is made by the
validator or its tests.

## Fixture-gold route diff

After a receipt passes the structural/provenance validator, an optional
stdlib-only helper can print raw routing-field differences against the pinned
fixture's `route_gold` values:

```bash
python3 analyze_routes.py report.json
python3 analyze_routes.py report.json --expected-config preregistered-config.json
python3 -m unittest -v test_analyze_routes.py
```

The helper rejects malformed, unpinned, incomplete, or otherwise invalid
receipts before comparing the five raw fields: `route_action`,
`selected_skill_ids`, `review_subject_ids`, `target_skill_ids`, and
`ingest_decision`. ID lists are compared exactly, including order. It reports
the case, condition, field, fixture value, and receipt value for each
difference. It does not compare answers or telemetry, calculate an aggregate
score, grade a skill, recommend a lifecycle action, or change installed skill
state. The fixture's gold values are synthetic hypotheses for these cases, not
universal policy. No model, provider, relay, or network is used by the helper or
its tests.

The parsed v1 JSON payload is also pinned by SHA-256 over UTF-8 canonical JSON
(sorted keys, compact separators, Unicode preserved). Any task, gold decision,
skill content/version/path, or protocol edit fails validation until the pin is
deliberately updated; changes to v1 semantics should also bump the schema
version. This hash protects fixture integrity only and grants no runtime
authorization.

## Raw verifier and telemetry summary

After structural validation, `analyze_outcomes.py` reports each case's recorded
`pass`, `fail`, or unavailable verifier status in all three conditions, plus
the raw focused-skill and small-pack transitions against that case's
`no_skill` status. It prints descriptive per-condition and transition counts;
it does not calculate an aggregate score or statistical significance.

Numeric telemetry differences are condition minus `no_skill` for the same
case. A delta is printed only when both paired receipt measurements have
values and evidence references; unavailable pairs are counted and omitted.
The evidence paths are references only: this tool does not open or authenticate
them. The receipt validator checks structure and declared provenance, not
whether a verifier label is correct. Synthetic fixture gold remains a test
hypothesis and is never reported as actual model performance or skill efficacy.

```bash
python3 analyze_outcomes.py report.json
python3 analyze_outcomes.py report.json --expected-config preregistered-config.json
python3 -m unittest -v test_analyze_outcomes.py
```

The analyzer and tests use Python's standard library and make no model,
provider, relay, filesystem-skill, or network calls.
