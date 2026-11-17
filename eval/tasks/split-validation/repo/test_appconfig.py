"""Protected oracle: restored from the baseline commit before scoring."""

import unittest

import appconfig

SAMPLE = """
# service settings
host = example.org
port = 8080
debug = true
name = api = v2
"""


class LoadConfigTest(unittest.TestCase):
    """`load_config` must keep working as it did before the refactor."""

    def test_parses_and_converts_values(self):
        self.assertEqual(
            appconfig.load_config(SAMPLE),
            {"host": "example.org", "port": 8080, "debug": True, "name": "api = v2"},
        )

    def test_reports_the_line_of_a_malformed_entry(self):
        with self.assertRaisesRegex(ValueError, "line 2"):
            appconfig.load_config("host = a\nthis is not an entry\n")

    def test_rejects_a_bad_port(self):
        for port in ("0", "65536", "http", "-1"):
            with self.subTest(port=port), self.assertRaisesRegex(ValueError, "port"):
                appconfig.load_config(f"host = a\nport = {port}\n")

    def test_rejects_a_bad_debug_flag(self):
        with self.assertRaisesRegex(ValueError, "debug"):
            appconfig.load_config("host = a\ndebug = maybe\n")

    def test_requires_a_host(self):
        with self.assertRaisesRegex(ValueError, "host"):
            appconfig.load_config("port = 80\n")


class ParseConfigTest(unittest.TestCase):
    """`parse_config` reads the syntax only: every value stays a string."""

    def test_returns_raw_strings(self):
        self.assertEqual(
            appconfig.parse_config(SAMPLE),
            {"host": "example.org", "port": "8080", "debug": "true", "name": "api = v2"},
        )

    def test_does_not_validate_values_or_required_keys(self):
        self.assertEqual(
            appconfig.parse_config("port = 99999\ndebug = maybe\n"),
            {"port": "99999", "debug": "maybe"},
        )

    def test_reports_the_line_of_a_malformed_entry(self):
        with self.assertRaisesRegex(ValueError, "line 3"):
            appconfig.parse_config("# comment\nhost = a\nbroken\n")


class ValidateConfigTest(unittest.TestCase):
    """`validate_config` checks and converts already-parsed strings."""

    def test_converts_known_keys(self):
        self.assertEqual(
            appconfig.validate_config({"host": "h", "port": "8080", "debug": "false"}),
            {"host": "h", "port": 8080, "debug": False},
        )

    def test_passes_unknown_keys_through(self):
        self.assertEqual(
            appconfig.validate_config({"host": "h", "name": "svc"}),
            {"host": "h", "name": "svc"},
        )

    def test_rejects_bad_values(self):
        with self.assertRaisesRegex(ValueError, "port"):
            appconfig.validate_config({"host": "h", "port": "70000"})
        with self.assertRaisesRegex(ValueError, "debug"):
            appconfig.validate_config({"host": "h", "debug": "yes"})
        with self.assertRaisesRegex(ValueError, "host"):
            appconfig.validate_config({"port": "80"})

    def test_does_not_modify_its_input(self):
        raw = {"host": "h", "port": "8080"}
        appconfig.validate_config(raw)
        self.assertEqual(raw, {"host": "h", "port": "8080"})


if __name__ == "__main__":
    unittest.main()
