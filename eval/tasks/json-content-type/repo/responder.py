"""HTTP-style response helpers."""


def json_response(payload, status=200):
    """Return `(status, headers, body)` for a JSON API response.

    TASK: the body is not JSON yet and no header says what it is. Implement the
    contract described in task.json; test_responder.py asserts it. Do not
    modify the test.
    """
    return status, {}, str(payload)
