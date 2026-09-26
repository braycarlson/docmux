"""Validates every JSON file in a directory against the Atlassian ADF JSON Schema.

Invoked by tests/adf_schema.rs; exits non-zero and prints each failing file with the first
validation error so the Rust test can surface it.
"""

import json
import pathlib
import sys

import jsonschema


def main() -> int:
    schema_path = pathlib.Path(sys.argv[1])
    directory = pathlib.Path(sys.argv[2])

    assert schema_path.is_file()
    assert directory.is_dir()

    schema = json.loads(schema_path.read_text())
    validator = jsonschema.Draft4Validator(schema)
    failures = 0
    checked = 0

    for path in sorted(directory.glob('*.json')):
        checked += 1

        try:
            document = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            failures += 1
            print(f'{path.name}: invalid JSON: {error}')

            continue

        error = jsonschema.exceptions.best_match(validator.iter_errors(document))

        if error is not None:
            failures += 1
            location = '/'.join(str(part) for part in error.absolute_path)
            print(f'{path.name}: at {location}: {error.message[:300]}')

    print(f'{checked} documents checked, {failures} failed')

    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
