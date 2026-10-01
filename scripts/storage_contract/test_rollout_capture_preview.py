"""Synthetic physical-copy and direct-lineage checks for snapshot previews."""

import json
import tempfile
import unittest
import uuid
from pathlib import Path

from .rollout_capture_preview import preview


class RolloutCapturePreviewTest(unittest.TestCase):
    def test_copies_and_direct_ancestors_are_reported_without_selecting_a_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            active = home / "sessions" / "2026" / "09" / "28"
            archive = home / "archived_sessions"
            active.mkdir(parents=True)
            archive.mkdir()
            parent = str(uuid.uuid4())
            child = str(uuid.uuid4())
            orphan = str(uuid.uuid4())
            compressed_child = str(uuid.uuid4())
            parent_name = f"rollout-2026-09-28T12-00-00-{parent}.jsonl"
            child_name = f"rollout-2026-09-28T12-00-01-{child}.jsonl"
            compressed_name = (
                f"rollout-2026-09-28T12-00-02-{compressed_child}.jsonl.zst"
            )
            (active / parent_name).write_text(self._meta(parent), encoding="utf-8")
            (archive / parent_name).write_text(self._meta(parent), encoding="utf-8")
            (active / f"{parent_name}.zst").write_bytes(b"synthetic compressed copy")
            (active / child_name).write_text(
                self._meta(child, parent), encoding="utf-8"
            )
            orphan_child = str(uuid.uuid4())
            (archive / f"rollout-2026-09-28T12-00-03-{orphan_child}.jsonl").write_text(
                self._meta(orphan_child, orphan), encoding="utf-8"
            )
            (active / compressed_name).write_bytes(b"synthetic compressed child")

            result = preview(home)

            self.assertEqual(result["duplicate_rollout_ids"]["examples"], [parent])
            self.assertEqual(result["plain_compressed_siblings"]["examples"], [parent])
            self.assertEqual(result["active_archive_copies"]["examples"], [parent])
            self.assertEqual(
                result["ambiguous_direct_ancestors"]["examples"], [f"{child}:{parent}"]
            )
            self.assertEqual(
                result["missing_direct_ancestors"]["examples"],
                [f"{orphan_child}:{orphan}"],
            )
            self.assertEqual(result["compressed_headers_unknown"], 2)
            self.assertFalse(result["capture_complete"])
            self.assertFalse(result["activation_permitted"])
            self.assertNotIn(str(home), json.dumps(result))
            self.assertNotIn("synthetic compressed", json.dumps(result))

    def test_symlink_is_not_followed_or_counted_as_canonical(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            active = home / "sessions" / "2026" / "09" / "28"
            active.mkdir(parents=True)
            outside = home / "outside"
            outside.mkdir()
            thread_id = str(uuid.uuid4())
            name = f"rollout-2026-09-28T12-00-00-{thread_id}.jsonl"
            (outside / name).write_text(self._meta(thread_id), encoding="utf-8")
            try:
                (active / name).symlink_to(outside / name)
            except (NotImplementedError, OSError):
                self.skipTest("symlink creation unavailable")

            result = preview(home)

            self.assertEqual(result["canonical_files"], 0)
            self.assertEqual(result["unknown_entries"], 1)

    @staticmethod
    def _meta(thread_id, ancestor=None):
        payload = {"id": thread_id}
        if ancestor is not None:
            payload["history_base"] = {"thread_id": ancestor}
        return json.dumps({"type": "session_meta", "payload": payload}) + "\n"
