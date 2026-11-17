"""Tiny `key = value` configuration loader."""


def load_config(text):
    """Parse and validate configuration text, returning a dict.

    TASK (refactor): reading the `key = value` syntax and checking the values
    are tangled together in one loop. Split them into `parse_config` and
    `validate_config` (see task.json); `load_config` must keep returning the
    same result. test_appconfig.py asserts all of it — do not modify it.
    """
    config = {}
    for number, line in enumerate(text.splitlines(), start=1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError(f"line {number}: expected key = value")
        key, value = (part.strip() for part in line.split("=", 1))
        if key == "port":
            if not value.isdigit() or not 1 <= int(value) <= 65535:
                raise ValueError(f"port must be a number from 1 to 65535, got {value!r}")
            config[key] = int(value)
        elif key == "debug":
            if value not in ("true", "false"):
                raise ValueError(f"debug must be true or false, got {value!r}")
            config[key] = value == "true"
        else:
            config[key] = value
    if "host" not in config:
        raise ValueError("host is required")
    return config
