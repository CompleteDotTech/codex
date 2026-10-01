import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import format_rust


class RustFormatterTests(unittest.TestCase):
    def test_empty_target_set_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                format_rust.workspace_targets(
                    {"workspace_root": directory, "packages": []},
                    Path(directory),
                )

    def test_utf16_quoting_and_nul(self):
        arguments = [
            "rustfmt.exe",
            "space name.rs",
            'quote"name.rs',
            "emoji\U0001f600.rs",
        ]
        expected = len(subprocess.list2cmdline(arguments).encode("utf-16-le")) // 2 + 1
        self.assertEqual(format_rust.command_units(arguments), expected)
        self.assertGreater(expected, len(subprocess.list2cmdline(arguments)))

    def test_batches_preserve_exact_union_options_and_budget(self):
        files = [
            f"C:/long directory/{number:04d}/" + "x" * 200 + ".rs"
            for number in range(400)
        ]
        options = [
            "--edition",
            "2024",
            "--config",
            "imports_granularity=Item",
            "--check",
        ]
        commands = format_rust.batches("C:/tools/rustfmt.exe", files, options)
        self.assertGreater(len(commands), 1)
        self.assertEqual(
            [path for command in commands for path in command[1 : -len(options)]], files
        )
        for command in commands:
            self.assertEqual(command[-len(options) :], options)
            self.assertLessEqual(format_rust.command_units(command), 32767)

    def test_single_oversized_target_refuses(self):
        with self.assertRaises(ValueError):
            format_rust.batches("rustfmt.exe", ["x" * 32767], ["--edition", "2024"])

    def test_all_target_kinds_canonical_dedup_first_edition_and_order(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            first, second = cwd / "a.rs", cwd / "z.rs"
            first.touch()
            second.touch()
            metadata = {
                "workspace_root": directory,
                "packages": [
                    {
                        "targets": [
                            {
                                "src_path": str(second),
                                "edition": "2024",
                                "kind": ["bench"],
                            },
                            {
                                "src_path": str(first),
                                "edition": "2021",
                                "kind": ["custom-build"],
                            },
                            {
                                "src_path": str(cwd / "." / "a.rs"),
                                "edition": "2018",
                                "kind": ["test"],
                            },
                        ]
                    }
                ],
            }
            self.assertEqual(
                format_rust.workspace_targets(metadata, cwd),
                {
                    "2021": [str(first.resolve())],
                    "2024": [str(second.resolve())],
                },
            )
            with self.assertRaises(ValueError):
                format_rust.workspace_targets(metadata, cwd.parent)

    def test_metadata_failure_never_launches_formatter(self):
        with patch(
            "format_rust.subprocess.check_output",
            side_effect=subprocess.CalledProcessError(1, "cargo"),
        ):
            with patch("format_rust.subprocess.run") as launch:
                with self.assertRaises(subprocess.CalledProcessError):
                    format_rust.run(Path.cwd(), [])
                launch.assert_not_called()

    def test_malformed_offline_result_uses_valid_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            path = cwd / "a.rs"
            path.touch()
            metadata = {
                "workspace_root": directory,
                "packages": [
                    {
                        "targets": [
                            {"src_path": str(path), "edition": "2024"},
                        ]
                    }
                ],
            }
            with patch.dict("format_rust.os.environ", {"RUSTFMT": "owned-rustfmt.exe"}):
                with patch(
                    "format_rust.subprocess.check_output",
                    side_effect=["{", json.dumps(metadata)],
                ):
                    with patch(
                        "format_rust.subprocess.run",
                        return_value=subprocess.CompletedProcess([], 0),
                    ):
                        self.assertEqual(format_rust.run(cwd, []), 0)

    def test_malformed_fallback_refuses_before_formatter(self):
        with patch("format_rust.subprocess.check_output", side_effect=["{", "{"]):
            with patch("format_rust.subprocess.run") as launch:
                with self.assertRaises(json.JSONDecodeError):
                    format_rust.run(Path.cwd(), [])
                launch.assert_not_called()

    def test_offline_metadata_failure_falls_back_without_lock_restriction(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            path = cwd / "a.rs"
            path.touch()
            metadata = {
                "workspace_root": directory,
                "packages": [
                    {
                        "targets": [
                            {"src_path": str(path), "edition": "2024"},
                        ]
                    }
                ],
            }
            with patch.dict("format_rust.os.environ", {"RUSTFMT": "owned-rustfmt.exe"}):
                with patch("format_rust.subprocess.check_output") as query:
                    query.side_effect = [
                        subprocess.CalledProcessError(1, "cargo"),
                        json.dumps(metadata),
                    ]
                    with patch(
                        "format_rust.subprocess.run",
                        return_value=subprocess.CompletedProcess([], 0),
                    ):
                        self.assertEqual(format_rust.run(cwd, []), 0)
                    self.assertEqual(query.call_args_list[0].args[0][-1], "--offline")
                    self.assertNotIn("--offline", query.call_args_list[1].args[0])
                    self.assertNotIn("--locked", query.call_args_list[1].args[0])

    def test_child_failure_is_retained_and_editions_options_cwd_forwarded(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            paths = [cwd / "b.rs", cwd / "a.rs"]
            for path in paths:
                path.touch()
            metadata = {
                "workspace_root": directory,
                "packages": [
                    {
                        "targets": [
                            {"src_path": str(paths[0]), "edition": "2024"},
                            {"src_path": str(paths[1]), "edition": "2021"},
                        ]
                    }
                ],
            }
            with patch.dict("format_rust.os.environ", {"RUSTFMT": "owned-rustfmt.exe"}):
                with patch(
                    "format_rust.subprocess.check_output",
                    return_value=json.dumps(metadata),
                ):
                    with patch("format_rust.subprocess.run") as launch:
                        launch.side_effect = [
                            subprocess.CompletedProcess([], 7),
                            subprocess.CompletedProcess([], 0),
                        ]
                        self.assertEqual(format_rust.run(cwd, ["--check"]), 7)
                        self.assertEqual(launch.call_count, 2)
                        for call, edition in zip(
                            launch.call_args_list, ["2021", "2024"]
                        ):
                            self.assertEqual(
                                call.args[0][-3:], ["--edition", edition, "--check"]
                            )
                            self.assertEqual(call.kwargs["cwd"], cwd)


if __name__ == "__main__":
    unittest.main()
