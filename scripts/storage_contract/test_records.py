import json
import random
import struct
import tracemalloc
import unittest

from .records import ContractError, Fingerprint, encode_row, validate_row


def digest(lines, domain="threads", schema=1):
    result = Fingerprint(domain, schema)
    for line in lines:
        result.feed(line)
    return result.count, result.hexdigest()


class RecordTests(unittest.TestCase):
    def test_types_and_precision_survive_round_trip(self):
        values = [
            None,
            True,
            False,
            -(1 << 63),
            (1 << 63) - 1,
            0.0,
            -0.0,
            float("inf"),
            float("-inf"),
            "\x00e\u0301é💾",
            b"\x00\xff",
        ]
        line = encode_row(b"record", values)
        self.assertEqual(validate_row(line), (b"record", line))
        parsed = json.loads(line)
        self.assertEqual(parsed["values"][4]["value"], str((1 << 63) - 1))
        self.assertNotEqual(parsed["values"][5], parsed["values"][6])

    def test_semantic_type_distinctions_change_digest(self):
        values = [None, False, 0, 0.0, -0.0, "0", b"0", "é", "e\u0301"]
        fingerprints = {digest([encode_row(b"key", [value])]) for value in values}
        self.assertEqual(len(fingerprints), len(values))

    def test_input_json_whitespace_does_not_change_logical_digest(self):
        line = encode_row(b"key", [7, "history"])
        spaced = json.dumps(json.loads(line), separators=(", ", ": ")).encode() + b"\n"
        self.assertEqual(digest([line]), digest([spaced]))

    def test_count_equality_does_not_hide_payload_corruption(self):
        before = digest([encode_row(b"key", ["acknowledged"])])
        after = digest([encode_row(b"key", ["corrupted"])])
        self.assertEqual(before[0], after[0])
        self.assertNotEqual(before, after)

    def test_domain_and_schema_are_digest_bound(self):
        lines = [encode_row(b"key", [1])]
        self.assertNotEqual(digest(lines), digest(lines, domain="queue"))
        self.assertNotEqual(digest(lines), digest(lines, schema=2))

    def test_empty_domain_still_has_schema_bound_fingerprint(self):
        self.assertNotEqual(digest([]), digest([], schema=2))
        self.assertEqual(digest([])[0], 0)

    def test_duplicate_and_decreasing_keys_are_rejected(self):
        for keys in ([b"a", b"a"], [b"b", b"a"]):
            with (
                self.subTest(keys=keys),
                self.assertRaisesRegex(ContractError, "unordered_key"),
            ):
                digest(encode_row(key, [1]) for key in keys)

    def test_ieee_nan_payload_bits_are_not_normalized(self):
        lines = []
        for bits in ("7ff8000000000001", "7ff8000000000002", "7ff0000000000001"):
            line = (
                json.dumps(
                    {"key": "01", "values": [{"type": "f64", "value": bits}]}
                ).encode()
                + b"\n"
            )
            canonical = validate_row(line)[1]
            self.assertEqual(json.loads(canonical)["values"][0]["value"], bits)
            lines.append(canonical)
        self.assertEqual(len({digest([line]) for line in lines}), len(lines))

    def test_malformed_records_have_payload_free_errors(self):
        secret = "PRIVATE_PASSWORD_SENTINEL"
        examples = [
            b'{"key":"01","key":"02","values":[]}\n',
            b'{"key":"01","values":[],"extra":0}\n',
            b'{"key":"","values":[]}\n',
            b'{"key":"01","values":[{"type":"i64","value":"-0"}]}\n',
            b'{"key":"01","values":[{"type":"i64","value":"9223372036854775808"}]}\n',
            b'{"key":"01","values":[{"type":"bytes","value":"Zh=="}]}\n',
            b'{"key":"01","values":[{"type":"text","value":"\\ud800"}]}\n',
            b'{"key":"01","values":[{"type":"bool","value":1}]}\n',
            b'{"key":"01","values":[{"type":"f64","value":NaN}]}\n',
            b'{"key":"01","values":[]}',
            (secret + "\n").encode(),
            b"[" * 4000 + b"\n",
        ]
        for line in examples:
            with (
                self.subTest(line=line[:40]),
                self.assertRaises(ContractError) as caught,
            ):
                validate_row(line)
            self.assertNotIn(secret, str(caught.exception))

    def test_schema_rejects_boolean_integer_coercion(self):
        for schema in (True, 0, -1, 1 << 64):
            with self.subTest(schema=schema), self.assertRaises(ContractError):
                Fingerprint("threads", schema)

    def test_large_and_unsupported_cells_are_rejected(self):
        for value in (["x" * (1 << 20)], [object()], [1] * 257, [1 << 63]):
            with self.subTest(count=len(value)), self.assertRaises(ContractError):
                encode_row(b"key", value)

    def test_generated_ieee_values_preserve_bits(self):
        rng = random.Random(9348)
        for _ in range(100):
            bits = rng.getrandbits(64).to_bytes(8, "big")
            value = struct.unpack(">d", bits)[0]
            row = json.loads(encode_row(b"key", [value]))
            self.assertEqual(row["values"][0]["value"], bits.hex())

    def test_streaming_fingerprint_does_not_retain_history(self):
        tracemalloc.start()
        try:
            count, _ = digest(
                encode_row(i.to_bytes(8, "big"), ["x" * 128]) for i in range(4000)
            )
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(count, 4000)
        self.assertLess(peak, 1 << 20)
