"""Exercises the named CI matrix's default Lean pin."""

import unittest

from scripts.check_ci_default_lean import check_default


WORKFLOW = """\
jobs:
  check:
    strategy:
      matrix:
        lean_version: >-
          ${{ fromJSON(inputs.full_matrix &&
            '["4.35.0", "4.36.0"]' || '["4.36.0"]') }}
"""


class DefaultLeanMatrixTest(unittest.TestCase):
    def test_matching_default(self):
        check_default(WORKFLOW, "4.36.0")

    def test_stale_fallback_fails_even_when_full_matrix_contains_head(self):
        stale = WORKFLOW.replace("|| '[\"4.36.0\"]'", "|| '[\"4.35.0\"]'")
        with self.assertRaisesRegex(
            ValueError, r"default Lean matrix is .*4\.35\.0.*expected .*4\.36\.0"
        ):
            check_default(stale, "4.36.0")

    def test_unrelated_job_does_not_supply_the_default(self):
        other_job = WORKFLOW.removeprefix("jobs:\n").replace("  check:", "  other:")
        wrong_expression = WORKFLOW.replace("inputs.full_matrix", "inputs.other_matrix")
        with self.assertRaisesRegex(ValueError, "expected a folded fromJSON"):
            check_default(wrong_expression + other_job, "4.36.0")

    def test_ambiguous_matrix_entry_fails(self):
        duplicate = WORKFLOW + WORKFLOW[WORKFLOW.index("        lean_version:") :]
        with self.assertRaisesRegex(ValueError, "exactly one 'lean_version' key"):
            check_default(duplicate, "4.36.0")


if __name__ == "__main__":
    unittest.main()
