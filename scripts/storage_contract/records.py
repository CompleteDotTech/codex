"""Type-preserving, ordered logical fingerprints for migration audit records."""

import base64
import hashlib
import json
import re
import struct
from collections.abc import Sequence

MAX_ROW_BYTES = 1 << 20
MAX_KEY_BYTES = 1024
MAX_CELLS = 256


class ContractError(ValueError):
    """A fixed diagnostic code without source data, paths, or credentials."""


def require(condition: bool, code: str) -> None:
    if not condition:
        raise ContractError(code)


def fields(value: object, names: set[str]) -> dict:
    require(type(value) is dict and set(value) == names, "invalid_fields")
    return value


def integer(value: object, minimum: int = 0, maximum: int = (1 << 63) - 1) -> int:
    require(type(value) is int and minimum <= value <= maximum, "invalid_integer")
    return value


def token(value: object, pattern: str) -> str:
    require(
        type(value) is str and re.fullmatch(pattern, value) is not None, "invalid_token"
    )
    return value


def _pairs(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_field")
        result[key] = value
    return result


def _number(value: str) -> int:
    require(len(value) <= 20, "invalid_integer")
    return int(value)


def _reject_number(_value: str) -> None:
    raise ContractError("non_integer_json_number")


def parse_json(data: bytes, limit: int) -> object:
    require(len(data) <= limit, "input_too_large")
    try:
        return json.loads(
            data.decode("utf-8"),
            object_pairs_hook=_pairs,
            parse_int=_number,
            parse_float=_reject_number,
            parse_constant=_reject_number,
        )
    except (UnicodeError, RecursionError, ValueError) as exc:
        if isinstance(exc, ContractError):
            raise
        raise ContractError("invalid_json") from None


def _canonical(value: object) -> bytes:
    return (
        json.dumps(
            value,
            sort_keys=True,
            ensure_ascii=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("ascii")
        + b"\n"
    )


def validate_row(line: bytes) -> tuple[bytes, bytes]:
    require(len(line) <= MAX_ROW_BYTES and line.endswith(b"\n"), "invalid_row_boundary")
    row = fields(parse_json(line, MAX_ROW_BYTES), {"key", "values"})
    key = bytes.fromhex(token(row["key"], rf"(?:[0-9a-f]{{2}}){{1,{MAX_KEY_BYTES}}}"))
    cells = row["values"]
    require(type(cells) is list and len(cells) <= MAX_CELLS, "invalid_cells")
    for cell in cells:
        fields(cell, {"type", "value"})
        kind, value = cell["type"], cell["value"]
        require(type(kind) is str, "invalid_cell_type")
        if kind == "null":
            require(value is None, "invalid_null")
        elif kind == "bool":
            require(type(value) is bool, "invalid_bool")
        elif kind == "i64":
            text = token(value, r"0|-?[1-9][0-9]{0,18}")
            integer(int(text), -(1 << 63))
        elif kind == "f64":
            token(
                value, r"[0-9a-f]{16}"
            )  # Exact IEEE-754 bits, including NaN payloads.
        elif kind == "text":
            require(type(value) is str, "invalid_text")
            try:
                value.encode("utf-8")
            except UnicodeError:
                raise ContractError("invalid_text") from None
        elif kind == "bytes":
            token(value, r"[A-Za-z0-9+/]*={0,2}")
            try:
                decoded = base64.b64decode(value, validate=True)
            except ValueError:
                raise ContractError("invalid_base64") from None
            require(
                base64.b64encode(decoded).decode("ascii") == value, "invalid_base64"
            )
        else:
            raise ContractError("invalid_cell_type")
    encoded = _canonical(row)
    require(len(encoded) <= MAX_ROW_BYTES, "canonical_row_too_large")
    return key, encoded


def encode_row(key: bytes, values: Sequence[object]) -> bytes:
    """Encode scalar values without coercing booleans, text, binary or integers."""
    require(type(key) is bytes and 0 < len(key) <= MAX_KEY_BYTES, "invalid_key")
    cells = []
    budget = len(_canonical({"key": key.hex(), "values": []}))
    for value in values:
        require(len(cells) < MAX_CELLS, "invalid_cells")
        kind = type(value)
        if value is None:
            tag, encoded = "null", None
        elif kind is bool:
            tag, encoded = "bool", value
        elif kind is int:
            integer(value, -(1 << 63))
            tag, encoded = "i64", str(value)
        elif kind is float:
            tag, encoded = "f64", struct.pack(">d", value).hex()
        elif kind is str:
            require(len(value) <= MAX_ROW_BYTES, "input_too_large")
            tag, encoded = "text", value
        elif kind is bytes:
            require(len(value) <= MAX_ROW_BYTES, "input_too_large")
            tag, encoded = "bytes", base64.b64encode(value).decode("ascii")
        else:
            raise ContractError("invalid_cell_type")
        cell = {"type": tag, "value": encoded}
        # The enclosing row already includes its LF; cells add commas, not LFs.
        budget += len(_canonical(cell)) - 1 + (1 if cells else 0)
        require(budget <= MAX_ROW_BYTES, "canonical_row_too_large")
        cells.append(cell)
    return validate_row(_canonical({"key": key.hex(), "values": cells}))[1]


class Fingerprint:
    """Incremental digest over strictly increasing byte keys and typed values.

    Adapters must supply portable keys and domain-defined timestamp units.
    Database collation, local paths and JSONL byte offsets are not portable keys.
    """

    def __init__(self, domain: str, schema: int):
        name = token(domain, r"[a-z][a-z0-9_.-]{0,127}").encode("ascii")
        integer(schema, 1)
        self._hash = hashlib.sha256(
            b"CDTX-logical-v1\0"
            + len(name).to_bytes(4, "big")
            + name
            + schema.to_bytes(8, "big")
        )
        self._last = b""
        self.count = 0

    def feed(self, line: bytes) -> None:
        key, encoded = validate_row(line)
        require(key > self._last, "duplicate_or_unordered_key")
        integer(self.count + 1)
        self._hash.update(len(encoded).to_bytes(8, "big"))
        self._hash.update(encoded)
        self._last = key
        self.count += 1

    def hexdigest(self) -> str:
        digest = self._hash.copy()
        digest.update(self.count.to_bytes(8, "big"))
        return digest.hexdigest()
