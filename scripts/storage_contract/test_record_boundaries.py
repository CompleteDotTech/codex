"""Exact serialized-size boundaries for the v1 logical-record contract."""

import base64
import json
import unittest
from unittest.mock import patch

from . import records
from .records import ContractError, MAX_CELLS, MAX_KEY_BYTES, MAX_ROW_BYTES
from .records import encode_row, validate_row


def canonical_row(key, cells):
    return (
        json.dumps(
            {"key": key.hex(), "values": cells},
            sort_keys=True,
            ensure_ascii=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("ascii")
        + b"\n"
    )


class RecordBoundaryTests(unittest.TestCase):
    def assert_boundary(self, key=b"k", prefix=(), text=""):
        # Build the oracle without encode_row or its internal size calculation.
        values = [value for value, _ in prefix]
        cells = [cell for _, cell in prefix]
        tail = {"type": "text", "value": text}
        padding = MAX_ROW_BYTES - len(canonical_row(key, [*cells, tail]))
        self.assertGreaterEqual(padding, 0)
        text += "x" * padding
        tail = {"type": "text", "value": text}
        expected = canonical_row(key, [*cells, tail])
        self.assertEqual(len(expected), MAX_ROW_BYTES)
        self.assertEqual(validate_row(expected), (key, expected))
        self.assertEqual(encode_row(key, [*values, text]), expected)

        oversized = canonical_row(key, [*cells, {"type": "text", "value": text + "x"}])
        self.assertEqual(len(oversized), MAX_ROW_BYTES + 1)
        with self.assertRaisesRegex(ContractError, "^invalid_row_boundary$"):
            validate_row(oversized)
        with self.assertRaisesRegex(ContractError, "^canonical_row_too_large$"):
            encode_row(key, [*values, text + "x"])

    def test_single_text_cell_includes_exact_envelope_and_newline(self):
        self.assert_boundary()

    def test_maximum_key_is_counted_as_hex_bytes(self):
        self.assert_boundary(key=bytes(range(256)) * (MAX_KEY_BYTES // 256))

    def test_maximum_cell_count_includes_exact_comma_count(self):
        self.assert_boundary(
            prefix=[(None, {"type": "null", "value": None})] * (MAX_CELLS - 1)
        )

    def test_mixed_scalar_tags_are_counted_without_type_coercion(self):
        self.assert_boundary(
            prefix=[
                (None, {"type": "null", "value": None}),
                (True, {"type": "bool", "value": True}),
                (False, {"type": "bool", "value": False}),
                (-(1 << 63), {"type": "i64", "value": "-9223372036854775808"}),
                ((1 << 63) - 1, {"type": "i64", "value": "9223372036854775807"}),
                (-0.0, {"type": "f64", "value": "8000000000000000"}),
            ]
        )

    def test_json_escaping_uses_serialized_bytes_not_text_length(self):
        for text in ("\x00" * 100_000, "💾" * 70_000, '"\\' * 100_000):
            with self.subTest(character=text[0]):
                self.assert_boundary(text=text)

    def test_binary_base64_padding_is_counted_exactly(self):
        for count in (700_000, 700_001, 700_002):
            data = b"\xff" * count
            with self.subTest(remainder=count % 3):
                self.assert_boundary(
                    prefix=[
                        (
                            data,
                            {
                                "type": "bytes",
                                "value": base64.b64encode(data).decode("ascii"),
                            },
                        )
                    ]
                )

    def test_valid_near_limit_rows_are_not_rejected_by_a_safety_estimate(self):
        overhead = len(canonical_row(b"k", [{"type": "text", "value": ""}]))
        for spare in (0, 1, 2, 40, 41, 42, 64):
            text = "x" * (MAX_ROW_BYTES - overhead - spare)
            expected = canonical_row(b"k", [{"type": "text", "value": text}])
            with self.subTest(spare=spare):
                self.assertEqual(len(expected), MAX_ROW_BYTES - spare)
                self.assertEqual(encode_row(b"k", [text]), expected)

    def test_oversized_cell_is_rejected_before_final_row_validation(self):
        overhead = len(canonical_row(b"k", [{"type": "text", "value": ""}]))
        text = "x" * (MAX_ROW_BYTES - overhead + 1)
        with patch.object(records, "validate_row") as validator:
            with self.assertRaisesRegex(ContractError, "^canonical_row_too_large$"):
                encode_row(b"k", [text])
            validator.assert_not_called()

    def test_empty_values_keep_the_existing_canonical_representation(self):
        key = b"\xff" * MAX_KEY_BYTES
        expected = canonical_row(key, [])
        self.assertEqual(encode_row(key, []), expected)
        self.assertEqual(validate_row(expected), (key, expected))

    def test_existing_cell_and_key_limits_are_not_relaxed(self):
        with self.assertRaisesRegex(ContractError, "^invalid_cells$"):
            encode_row(b"k", [None] * (MAX_CELLS + 1))
        for key in (b"", b"k" * (MAX_KEY_BYTES + 1)):
            with self.subTest(length=len(key)):
                with self.assertRaisesRegex(ContractError, "^invalid_key$"):
                    encode_row(key, [])
