"""Protected oracle: restored from the baseline commit before scoring."""

import json
import unittest

from responder import json_response


class JsonResponseTest(unittest.TestCase):
    def test_body_is_the_json_encoding_of_the_payload(self):
        payload = {"ok": True, "items": [1, 2, 3], "name": None}
        _status, _headers, body = json_response(payload)
        self.assertIsInstance(body, str)
        self.assertEqual(json.loads(body), payload)

    def test_declares_the_json_content_type(self):
        _status, headers, _body = json_response({"ok": True})
        self.assertEqual(headers["Content-Type"], "application/json")

    def test_status_defaults_to_200(self):
        status, _headers, _body = json_response([])
        self.assertEqual(status, 200)

    def test_keeps_an_explicit_status(self):
        status, headers, body = json_response({"error": "not found"}, status=404)
        self.assertEqual(status, 404)
        self.assertEqual(headers["Content-Type"], "application/json")
        self.assertEqual(json.loads(body), {"error": "not found"})

    def test_encodes_a_top_level_list(self):
        _status, _headers, body = json_response(["a", "b"])
        self.assertEqual(json.loads(body), ["a", "b"])


if __name__ == "__main__":
    unittest.main()
