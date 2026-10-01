"""Source-faithful synthetic path rows and offline relocation decisions."""

import hashlib
import json
import sqlite3
import unittest
from contextlib import closing

from .records import ContractError
from .relocation import preview_relocation
from .source_catalog import verified_migrations


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


class RelocationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with closing(sqlite3.connect(":memory:")) as connection:
            connection.execute("PRAGMA foreign_keys=ON")
            for script in verified_migrations("state_5.sqlite"):
                connection.executescript(script.decode())
            connection.execute(
                "INSERT INTO projects (id,name,position,created_at_ms,updated_at_ms) "
                "VALUES ('project1','synthetic',0,1,1)"
            )
            connection.execute(
                "INSERT INTO project_roots (project_id,position,path) VALUES (?,?,?)",
                ("project1", 0, r"C:\source\workspace"),
            )
            for thread_id, cwd, rollout in (
                ("thread1", r"C:\source\workspace\sub", r"C:\source\sessions\a.jsonl"),
                ("thread2", "/source/workspace", "/source/sessions/b.jsonl"),
            ):
                connection.execute(
                    "INSERT INTO threads (id,rollout_path,created_at,updated_at,source,"
                    "model_provider,cwd,title,sandbox_policy,approval_mode) "
                    "VALUES (?,?,1,1,'cli','synthetic',?,'','{}','never')",
                    (thread_id, rollout, cwd),
                )
            cls.rows = []
            for thread_id, cwd, rollout in connection.execute(
                "SELECT id,cwd,rollout_path FROM threads ORDER BY id"
            ):
                flavor = "windows" if thread_id == "thread1" else "posix"
                cls.rows.extend(
                    [
                        {
                            "kind": "thread_cwd",
                            "id": thread_id,
                            "source_path": cwd,
                            "flavor": flavor,
                        },
                        {
                            "kind": "rollout_path",
                            "id": thread_id,
                            "source_path": rollout,
                            "flavor": flavor,
                        },
                    ]
                )
            project_id, position, root = connection.execute(
                "SELECT project_id,position,path FROM project_roots"
            ).fetchone()
            cls.rows.append(
                {
                    "kind": "project_root",
                    "id": f"{project_id}.{position}",
                    "source_path": root,
                    "flavor": "windows",
                }
            )

    def setUp(self):
        self.source_host = "00000000-0000-4000-8000-000000000001"
        self.target_host = "00000000-0000-4000-8000-000000000002"
        self.dataset = "00000000-0000-4000-8000-000000000003"
        self.plan = {
            "version": 1,
            "source_host_id": self.source_host,
            "target_host_id": self.target_host,
            "dataset_id": self.dataset,
            "records": [
                {
                    **row,
                    "portable_rollout_id": "00000000-0000-4000-8000-000000000004"
                    if row["kind"] == "rollout_path" and row["id"] == "thread1"
                    else "00000000-0000-4000-8000-000000000005"
                    if row["kind"] == "rollout_path"
                    else None,
                }
                for row in self.rows
            ],
            "mappings": [
                {
                    "source_root": r"C:\source\workspace",
                    "source_flavor": "windows",
                    "target_root": "/target/workspace",
                    "target_flavor": "posix",
                },
                {
                    "source_root": "/source/workspace",
                    "source_flavor": "posix",
                    "target_root": "/target/other",
                    "target_flavor": "posix",
                },
            ],
        }

    def preview(self):
        raw = encoded(self.plan)
        return preview_relocation(
            raw,
            hashlib.sha256(raw).hexdigest(),
            self.source_host,
            self.target_host,
            self.dataset,
            self.rows,
        )

    def test_cross_platform_rows_keep_identity_without_source_path_use(self):
        result = self.preview()
        by_kind = {kind: [] for kind in ("thread_cwd", "project_root", "rollout_path")}
        for item in result["dispositions"]:
            by_kind[item["kind"]].append(item["disposition"])
        self.assertEqual(
            by_kind["thread_cwd"], ["mapping_candidate", "mapping_candidate"]
        )
        self.assertEqual(by_kind["project_root"], ["mapping_candidate"])
        self.assertEqual(
            by_kind["rollout_path"],
            ["portable_id_source_path_only", "portable_id_source_path_only"],
        )
        self.assertFalse(result["resume_permitted"])
        self.assertFalse(result["activation_permitted"])
        for source in self.rows:
            self.assertNotIn(source["source_path"], str(result))

    def test_missing_mapping_is_unresolved(self):
        self.plan["mappings"].clear()
        dispositions = self.preview()["dispositions"]
        self.assertEqual(
            [
                item["disposition"]
                for item in dispositions
                if item["kind"] != "rollout_path"
            ],
            ["unresolved", "unresolved", "unresolved"],
        )

    def test_windows_mapping_requires_exact_components(self):
        self.plan["records"] = [self.plan["records"][0]]
        self.rows = [self.rows[0]]
        self.plan["mappings"] = [self.plan["mappings"][0]]
        expected = {
            "status": "offline_relocation_preview",
            "activation_permitted": False,
            "resume_permitted": False,
            "source_observation_verified": False,
            "dispositions": [
                {"kind": "thread_cwd", "id": "thread1", "disposition": "unresolved"}
            ],
        }
        for root, disposition in (
            (r"c:\source\workspace", "unresolved"),
            (r"C:\Source\workspace", "unresolved"),
            (r"C:\source\Workspace", "unresolved"),
            (r"C:\source\work", "unresolved"),
            (r"C:\source\workspace\sub\child", "unresolved"),
            (r"C:\source\workspace", "mapping_candidate"),
            ("C:/source/workspace", "mapping_candidate"),
            (r"C:\source\workspace\sub", "mapping_candidate"),
            ("C:\\", "mapping_candidate"),
        ):
            with self.subTest(root=root):
                self.plan["mappings"][0]["source_root"] = root
                expected["dispositions"][0]["disposition"] = disposition
                self.assertEqual(self.preview(), expected)

    def test_invalid_windows_components_are_rejected_before_mapping(self):
        components = [
            *[f"bad{character}name" for character in '<>:"|?*\x00\x01\x1f'],
            "trailing.",
            "trailing ",
            "nul",
            "NUL.tar.gz",
            "con .txt",
            "COM1",
            "COM².txt",
            "LPT9",
            "LPT³",
            "CONIN$",
            "CONOUT$",
            "C:",
        ]
        for component in components:
            for location in ("record", "source_root", "target_root", "suffix"):
                with self.subTest(component=component, location=location):
                    record = {
                        **self.plan["records"][0],
                        "source_path": r"C:\source\valid",
                        "flavor": "windows",
                    }
                    mapping = {
                        "source_root": r"C:\source",
                        "source_flavor": "windows",
                        "target_root": r"C:\target",
                        "target_flavor": "windows",
                    }
                    if location == "record":
                        record["source_path"] = f"C:\\source\\{component}\\child"
                    elif location == "suffix":
                        record.update(
                            source_path=f"/source/{component}/child", flavor="posix"
                        )
                        mapping.update(source_root="/source", source_flavor="posix")
                    else:
                        mapping[location] = f"C:\\{component}\\child"
                    self.plan["records"] = [record]
                    self.rows = [
                        {
                            key: value
                            for key, value in record.items()
                            if key != "portable_rollout_id"
                        }
                    ]
                    self.plan["mappings"] = [mapping]
                    with self.assertRaises(ContractError) as failure:
                        self.preview()
                    self.assertEqual(str(failure.exception), "invalid_path")

    def test_valid_windows_targets_preserve_mapping_candidates(self):
        expected = self.preview()
        for root in (
            "D:\\",
            r"D:\target\COM10\NULdata\.hidden\part name",
            r"D:\target\café\项目",
        ):
            with self.subTest(root=root):
                for mapping in self.plan["mappings"]:
                    mapping.update(target_root=root, target_flavor="windows")
                self.assertEqual(self.preview(), expected)

    def test_overlapping_roots_and_traversal_are_rejected(self):
        self.plan["mappings"].append(
            {
                "source_root": r"C:\source",
                "source_flavor": "windows",
                "target_root": "/target/ambiguous",
                "target_flavor": "posix",
            }
        )
        with self.assertRaisesRegex(ContractError, "ambiguous_mapping"):
            self.preview()
        self.plan["mappings"].pop()
        self.plan["records"][0]["source_path"] = r"C:\source\workspace\..\outside"
        with self.assertRaisesRegex(ContractError, "path_traversal"):
            self.preview()

    def test_wrong_identity_or_source_row_is_rejected_without_path_echo(self):
        self.plan["target_host_id"] = "00000000-0000-4000-8000-000000000099"
        with self.assertRaisesRegex(ContractError, "identity_mismatch"):
            self.preview()
        self.plan["target_host_id"] = self.target_host
        self.plan["records"][0]["source_path"] = r"C:\source\private"
        with self.assertRaises(ContractError) as failure:
            self.preview()
        self.assertNotIn("private", str(failure.exception))

    def test_modified_plan_fails_independent_digest(self):
        raw = encoded(self.plan)
        self.plan["dataset_id"] = "00000000-0000-4000-8000-000000000099"
        with self.assertRaisesRegex(ContractError, "plan_digest_mismatch"):
            preview_relocation(
                encoded(self.plan),
                hashlib.sha256(raw).hexdigest(),
                self.source_host,
                self.target_host,
                self.dataset,
                self.rows,
            )
