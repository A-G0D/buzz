import assert from "node:assert/strict";
import test from "node:test";

import { possibleSkillOverlaps } from "./skillOverlap.ts";

const selected = {
  name: "api-review",
  description: "Review API migration plans",
  contentHash: "selected-hash",
  validationError: null,
  runtimeCompatibility: [],
  content:
    "---\nname: api-review\ndescription: Review API migration plans\n---\n\n# API migration checklist\n\nCheck release readiness.\n",
};

test("possible skill overlaps uses only exact terms and identical content hashes", () => {
  const candidates = [
    {
      name: "migration-check",
      description: "API migration review",
      contentHash: "other-hash",
      validationError: null,
    },
    {
      name: "renamed-copy",
      description: "Unrelated title",
      contentHash: "selected-hash",
      validationError: null,
    },
    {
      name: "review-only",
      description: "Review unrelated work",
      contentHash: "different-hash",
      validationError: null,
    },
  ];

  assert.deepEqual(possibleSkillOverlaps(selected, candidates), [
    {
      name: "migration-check",
      sharedTerms: ["api", "migration", "review"],
      sameContentHash: false,
    },
    {
      name: "renamed-copy",
      sharedTerms: [],
      sameContentHash: true,
    },
  ]);
});

test("possible skill overlap normalizes Unicode and does not stem words", () => {
  const matching = {
    ...selected,
    name: "normalized-match",
    description: "API migration",
    contentHash: "different-hash",
  };
  const nonMatching = {
    ...selected,
    name: "reviews-migration",
    description: "APIs migrations",
    contentHash: "different-hash",
  };
  const source = {
    ...selected,
    name: "normalized-source",
    description: "Check release",
    content:
      "---\nname: normalized-source\ndescription: Check release\n---\n\n# ＡＰＩ migration\n",
  };

  assert.equal(possibleSkillOverlaps(source, [matching]).length, 1);
  assert.deepEqual(possibleSkillOverlaps(source, [nonMatching]), []);
});
