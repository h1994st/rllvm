import json
from pathlib import Path

import pytest

from benchmarks.records import (
    IncompleteRecordError,
    RecordError,
    append_record,
    read_json,
    read_records,
    write_json,
)


class TestRecords:
    @pytest.fixture(autouse=True)
    def _record_root(self, tmp_path: Path) -> None:
        self.root = tmp_path

    def test_write_json_atomically_replaces_existing_record(self) -> None:
        path = self.root / "result.json"
        path.write_text('{"schema_version": 1, "old": true}\n')

        write_json(path, {"schema_version": 1, "value": 12})

        assert json.loads(path.read_text()) == {
            "schema_version": 1,
            "value": 12,
        }
        assert path.read_bytes().endswith(b"\n")

    def test_failed_json_encoding_preserves_existing_record(self) -> None:
        path = self.root / "result.json"
        original = b'{"schema_version": 1, "value": 3}\n'
        path.write_bytes(original)

        with pytest.raises(TypeError):
            write_json(path, {"schema_version": 1, "bad": object()})

        assert path.read_bytes() == original

    def test_append_and_read_records_preserve_each_observation(self) -> None:
        path = self.root / "observations.jsonl"

        append_record(path, {"schema_version": 1, "sample": 1})
        append_record(path, {"schema_version": 1, "sample": 2})

        assert read_records(path) == [
            {"schema_version": 1, "sample": 1},
            {"schema_version": 1, "sample": 2},
        ]

    def test_read_json_rejects_a_malformed_record(self) -> None:
        path = self.root / "result.json"
        path.write_text('{"schema_version": 1,')

        with pytest.raises(RecordError, match="malformed"):
            read_json(path)

    def test_read_records_rejects_unknown_schema_version(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_text('{"schema_version": 2}\n')

        with pytest.raises(RecordError, match="schema version 2"):
            read_records(path)

    def test_read_records_reports_an_interrupted_final_line(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_bytes(
            b'{"schema_version": 1, "sample": 1}\n'
            b'{"schema_version": 1, "sample":'
        )

        with pytest.raises(
            IncompleteRecordError, match="incomplete final record"
        ):
            read_records(path)

    def test_read_records_rejects_non_object_records(self) -> None:
        path = self.root / "observations.jsonl"
        path.write_text("[1, 2, 3]\n")

        with pytest.raises(RecordError, match="JSON object"):
            read_records(path)
