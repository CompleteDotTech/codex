"""Adversarial provenance and independent policy generation tests."""

import hashlib
import json
import shutil
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .records import ContractError
from .snapshot_test_support import make_fixture, policy_for
from .source_catalog import FIXTURES, STORES, build_fixture_policy, verified_migrations


class SourceCatalogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.assets = self.root / "fixtures"
        shutil.copytree(FIXTURES, self.assets)

    def rewrite_provenance(self, mutate):
        path = self.assets / "PROVENANCE.json"
        content = json.loads(path.read_bytes())
        mutate(content)
        path.write_text(json.dumps(content))

    def test_all_available_catalog_policies_open_their_independent_source_schemas(self):
        for store in STORES:
            with self.subTest(store=store), closing(sqlite3.connect(":memory:")) as db:
                scripts = verified_migrations(store)
                for version, script in enumerate(scripts, 1):
                    db.executescript(script.decode())
                    self.assertEqual(
                        json.loads(build_fixture_policy(store, version=version)),
                        json.loads(policy_for(db)),
                    )

    def test_prior_board_and_queue_fixtures_match_pinned_policies(self):
        for kind, store in (
            ("queue", "queue_1.sqlite"),
            ("board", "agent_message_board_1.sqlite"),
        ):
            with self.subTest(store=store):
                actual = make_fixture(self.root / store, kind)
                expected = build_fixture_policy(
                    store, version=len(verified_migrations(store))
                )
                self.assertEqual(json.loads(actual), json.loads(expected))

    def test_unknown_primary_store_cannot_be_reported_as_an_empty_supported_schema(
        self,
    ):
        with self.assertRaisesRegex(ContractError, "unsupported_store_schema"):
            build_fixture_policy("state_5.sqlite", version=1)

    def test_unknown_or_boolean_versions_are_rejected(self):
        for version in (0, 3, True, "2", None):
            with self.subTest(version=version), self.assertRaises(ContractError):
                build_fixture_policy("queue_1.sqlite", version=version)

    def test_changed_source_bytes_are_rejected(self):
        (self.assets / "queue_0001.sql").write_text("SELECT 'unexpected';")
        with self.assertRaisesRegex(ContractError, "source_asset_mismatch"):
            verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_rewritten_checksums_cannot_replace_the_pinned_git_tree(self):
        content = (self.assets / "queue_0001.sql").read_bytes() + b"\n-- altered\n"
        (self.assets / "queue_0001.sql").write_bytes(content)

        def mutate(provenance):
            entry = next(
                e for e in provenance["assets"] if e["file"] == "queue_0001.sql"
            )
            entry["sha256"] = hashlib.sha256(content).hexdigest()
            entry["source_blob"] = hashlib.sha1(
                b"blob " + str(len(content)).encode() + b"\0" + content
            ).hexdigest()

        self.rewrite_provenance(mutate)
        with self.assertRaisesRegex(ContractError, "source_tree_mismatch"):
            verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_missing_or_duplicate_migration_entries_fail(self):
        path = self.assets / "PROVENANCE.json"
        original = path.read_bytes()
        for action in ("remove", "duplicate"):
            with self.subTest(action=action):
                path.write_bytes(original)

                def mutate(provenance):
                    entries = provenance["assets"]
                    selected = next(e for e in entries if e["file"] == "queue_0001.sql")
                    if action == "remove":
                        entries.remove(selected)
                    else:
                        entries.append(selected.copy())

                self.rewrite_provenance(mutate)
                with self.assertRaisesRegex(ContractError, "source_inventory_mismatch"):
                    verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_asset_path_escape_is_not_read(self):
        def mutate(provenance):
            entry = next(
                e for e in provenance["assets"] if e["file"] == "queue_0001.sql"
            )
            entry["file"] = "../not-owned.sql"

        self.rewrite_provenance(mutate)
        with self.assertRaisesRegex(ContractError, "source_inventory_mismatch"):
            verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_changed_source_identity_fails(self):
        self.rewrite_provenance(lambda data: data.update(commit="0" * 40))
        with self.assertRaisesRegex(ContractError, "source_provenance_mismatch"):
            verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_changed_board_extraction_fails(self):
        (self.assets / "board_schema.sql").write_text("CREATE TABLE unrelated(x);")
        with self.assertRaisesRegex(ContractError, "source_asset_mismatch"):
            verified_migrations("agent_message_board_1.sqlite", directory=self.assets)

    def test_bookkeeping_is_not_silently_assumed_compatible(self):
        with closing(sqlite3.connect(":memory:")) as db:
            for script in verified_migrations("queue_1.sqlite"):
                db.executescript(script.decode())
            expected = json.loads(build_fixture_policy("queue_1.sqlite", version=2))
            db.execute("CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY)")
            self.assertNotEqual(json.loads(policy_for(db)), expected)

    def test_board_source_identity_is_validated_too(self):
        self.rewrite_provenance(lambda data: data.update(commit="0" * 40))
        with self.assertRaisesRegex(ContractError, "source_provenance_mismatch"):
            verified_migrations("agent_message_board_1.sqlite", directory=self.assets)

    def test_malformed_inventory_does_not_get_inferred(self):
        self.rewrite_provenance(
            lambda data: data.update(assets=[{"file": "queue_0001.sql"}])
        )
        with self.assertRaises(ContractError):
            verified_migrations("queue_1.sqlite", directory=self.assets)

    def test_unknown_source_metadata_fields_are_rejected(self):
        self.rewrite_provenance(lambda data: data.update(unexpected="not trusted"))
        with self.assertRaises(ContractError):
            verified_migrations("queue_1.sqlite", directory=self.assets)
