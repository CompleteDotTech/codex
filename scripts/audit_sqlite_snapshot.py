#!/usr/bin/env python3
"""Read-only auditing of an operator-owned, offline SQLite backup artifact."""

from storage_contract.snapshot_cli import main

if __name__ == "__main__":
    raise SystemExit(main())
