import copy
import json
import unittest
from pathlib import Path

from validate_dataset import CONDITIONS, validate


DATASET = json.loads(Path(__file__).with_name("dataset.json").read_text(encoding="utf-8"))


class DatasetValidationTests(unittest.TestCase):
    def test_fixture_defines_a_complete_paired_matrix(self):
        self.assertEqual(validate(DATASET), [])
        self.assertEqual(len(DATASET["cases"]) * len(CONDITIONS), 24)

    def test_task_text_is_covered_by_the_payload_pin(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][0]["task"] += " Keep this answer concise."
        self.assertTrue(any("payload SHA-256" in error for error in validate(changed)))

    def test_valid_gold_route_change_is_covered_by_the_payload_pin(self):
        changed = copy.deepcopy(DATASET)
        gold = changed["cases"][0]["route_gold"]
        gold["route_action"] = "use_existing"
        gold["selected_skill_ids"] = ["exact_money"]
        self.assertTrue(any("payload SHA-256" in error for error in validate(changed)))

    def test_approved_skill_version_and_path_are_covered_by_the_payload_pin(self):
        changed = copy.deepcopy(DATASET)
        staged = changed["skills"]["timeline_normalize"]
        staged["version"] = "0.1.1"
        staged["staged_path"] = "skills/proposed/timeline_normalize-v2.md"
        changed["cases"][-1]["answer_contract"]["path"] = staged["staged_path"]
        self.assertTrue(any("payload SHA-256" in error for error in validate(changed)))

    def test_missing_condition_breaks_pair_completeness(self):
        changed = copy.deepcopy(DATASET)
        del changed["conditions"]["small_pack"]
        self.assertTrue(any("complete pairing" in error for error in validate(changed)))

    def test_contradictory_condition_description_is_rejected(self):
        changed = copy.deepcopy(DATASET)
        changed["conditions"]["no_skill"] += " Include all candidate skill bodies too."
        self.assertTrue(any("exact documented body-injection protocol" in error for error in validate(changed)))

    def test_changed_case_id_is_rejected_even_when_well_formed(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][0]["id"] = "one-off-addition-renamed"
        self.assertTrue(any("pinned case list" in error for error in validate(changed)))

    def test_duplicate_case_id_is_rejected(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][1]["id"] = changed["cases"][0]["id"]
        self.assertTrue(any("duplicate case ID" in error for error in validate(changed)))

    def test_unknown_candidate_and_bad_route_label_are_rejected(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][0]["small_pack_skill_ids"][0] = "missing_skill"
        changed["cases"][0]["route_gold"]["route_action"] = "install_now"
        errors = validate(changed)
        self.assertTrue(any("unknown skill" in error for error in errors))
        self.assertTrue(any("unknown route_action" in error for error in errors))

    def test_import_gate_is_the_only_true_ingest_decision(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][0]["route_gold"]["ingest_decision"] = True
        self.assertTrue(any("true only for import_approved" in error for error in validate(changed)))

    def test_approved_import_targets_a_staged_skill(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][-1]["route_gold"]["target_skill_ids"] = []
        self.assertTrue(any("target exactly timeline_normalize" in error for error in validate(changed)))

    def test_staged_path_rejects_blank_and_traversal(self):
        for path in ("", "../outside.md", "skills/../outside.md", "/absolute/file.md", r"skills\\outside.md"):
            with self.subTest(path=path):
                changed = copy.deepcopy(DATASET)
                changed["skills"]["timeline_normalize"]["staged_path"] = path
                self.assertTrue(any("safe, nonempty repo-relative" in error for error in validate(changed)))

    def test_approved_path_must_match_staged_record(self):
        changed = copy.deepcopy(DATASET)
        changed["cases"][-1]["answer_contract"]["path"] = "skills/proposed/other.md"
        self.assertTrue(any("answer path must match" in error for error in validate(changed)))

    def test_unreviewed_listing_cannot_smuggle_a_body(self):
        changed = copy.deepcopy(DATASET)
        changed["skills"]["external_unknown"]["body"] = "unreviewed"
        self.assertTrue(any("must not include a skill body" in error for error in validate(changed)))


if __name__ == "__main__":
    unittest.main()
