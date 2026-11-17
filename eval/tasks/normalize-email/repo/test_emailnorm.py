"""Protected oracle: restored from the baseline commit before scoring."""

import unittest

from emailnorm import normalize_email


class NormalizeEmailTest(unittest.TestCase):
    def test_trims_surrounding_whitespace(self):
        self.assertEqual(normalize_email("  ada@example.com \n"), "ada@example.com")

    def test_lowercases_the_whole_address(self):
        self.assertEqual(
            normalize_email("Ada.Lovelace@Example.COM"), "ada.lovelace@example.com"
        )

    def test_leaves_a_canonical_address_unchanged(self):
        self.assertEqual(normalize_email("grace@navy.mil"), "grace@navy.mil")

    def test_rejects_an_address_without_an_at_sign(self):
        with self.assertRaises(ValueError):
            normalize_email("ada.example.com")

    def test_rejects_blank_input(self):
        with self.assertRaises(ValueError):
            normalize_email("   ")


if __name__ == "__main__":
    unittest.main()
