import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from benchmarks.records import (
    IncompleteRecordError,
    RecordError,
    append_record,
    read_json,
    read_records,
    write_json,
)


class RecordsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def test_write_json_atomically_replaces_existing_record(self) -> None:
        path = self.root / "result.json"
        path.write_text('{"schema_version": 1, "old": true}\n')

        write_json(path, {"schema_version": 1, "value": 12})

        self.assertEqual(
            json.loads(path.read_text()),
            {"schema_version": 1, "value": 12},
        )
        self.assertTrue(path.read_bytes().endswith(b"\n"))

    def test_failed_json_encoding_preserves_existing_record(self) -> None:
        path = self.root / "result.json"
        original = b'{"schema_version": 1, "value": 3}\n'
        path.write_bytes(original)

        with self.assertRaises(TypeError):
            write_json(path, {"schema_version": 1, "bad": object()})

        self.assertEqual(path.read_bytes(), original)

    def test_append_and_read_records_preserve_each_observation(self) -> None:
        path = self.root / "observations.jsonl"

        append_record(path, {"schema_version": 1, "sample": 1})
        append_record(path, {"schema_version": 1, "sample": 2})

        self.assertEqual(
            read_records(path),
            [
                {"schema_version": 1, "sample": 1},
                {"schema_version": 1, "sample": 2},
            ],
        )

    def test_read_json_rejects_a_malformed_record(self) -> None:
        path = self.root / "result.json"
        path.write_text('{"schema_version": 1,')

        with self.assertRaisesRegex(RecordError, "malformed"):
            read_json(path)

    def test_read_records_rejects_unknown_schema_version(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_text('{"schema_version": 2}\n')

        with self.assertRaisesRegex(RecordError, "schema version 2"):
            read_records(path)

    def test_read_records_reports_an_interrupted_final_line(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_bytes(
            b'{"schema_version": 1, "sample": 1}\n'
            b'{"schema_version": 1, "sample":'
        )

        with self.assertRaisesRegex(
            IncompleteRecordError, "incomplete final record"
        ):
            read_records(path)

    def test_read_records_rejects_non_object_records(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_text("[1, 2, 3]\n")

        with self.assertRaisesRegex(RecordError, "JSON object"):
            read_records(path)


if __name__ == "__main__":
    unittest.main()
