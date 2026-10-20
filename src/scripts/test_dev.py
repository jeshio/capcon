# SPDX-License-Identifier: AGPL-3.0-or-later
"""Safety and policy tests for the local build cache driver; no real cache touched."""

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader("capcon_dev", str(Path(__file__).resolve().parents[1] / "dev"))
spec = importlib.util.spec_from_loader(loader.name, loader)
dev = importlib.util.module_from_spec(spec)
loader.exec_module(dev)


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.profile = self.root / "target" / "quick"
        self.incremental = self.profile / "incremental"
        self.incremental.mkdir(parents=True)
        self.policy = {"incremental_budget_gib": 8, "unused_days": 14, "minimum_free_gib": 0}

    def cache(self, name, age=0):
        path = self.incremental / name
        path.mkdir()
        file = path / "query-cache.bin"
        file.write_bytes(b"x" * 4096)
        modified = time.time() - age * 86400
        for item in (file, path):
            os.utime(item, (modified, modified))
        return path

    def keep(self, path):
        (self.profile / ".capcon-hot-cache.json").write_text(json.dumps({"directory": path.name}))

    def maintain(self, trim=True, remember=False):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            dev.maintain(self.root, self.policy, trim=trim, remember=remember)
        return output.getvalue()

    def test_budget_evicts_oldest_variant_and_keeps_the_working_ui(self):
        hot = self.cache("concat-hot", age=3)
        old = self.cache("concat_speech-old", age=2)
        recent = self.cache("concat_speech-new", age=1)
        one = (hot / "query-cache.bin").stat().st_blocks * 512
        self.policy["incremental_budget_gib"] = 2 * one / dev.GIB
        self.keep(hot)
        self.maintain()
        self.assertTrue(hot.exists())
        self.assertTrue(recent.exists())
        self.assertFalse(old.exists())

    def test_age_expiry_keeps_pinned_ui_and_recent_engine_cache(self):
        hot = self.cache("concat-hot", age=30)
        old = self.cache("concat_media-old", age=15)
        recent = self.cache("concat_media-new", age=1)
        self.keep(hot)
        self.maintain()
        self.assertTrue(hot.exists() and recent.exists())
        self.assertFalse(old.exists())

    def test_status_does_not_remove_caches(self):
        old = self.cache("concat_media-old", age=100)
        self.policy["incremental_budget_gib"] = 0
        self.maintain(trim=False)
        self.assertTrue(old.exists())

    def test_pressure_keeps_latest_complete_ui_snapshot_and_ignores_working_sessions(self):
        hot = self.cache("concat-hot")
        self.keep(hot)
        for name in ("s-1-a-old", "s-2-b-new", "s-3-c-working"):
            session = hot / name
            session.mkdir()
            (session / "query-cache.bin").write_bytes(b"x" * 4096)
        self.policy["incremental_budget_gib"] = 0
        self.maintain()
        self.assertFalse((hot / "s-1-a-old").exists())
        self.assertTrue((hot / "s-2-b-new").exists())
        self.assertTrue((hot / "s-3-c-working").exists())

    def test_complete_ui_snapshots_are_untouched_when_below_budget(self):
        hot = self.cache("concat-hot")
        for name in ("s-1-a-old", "s-2-b-new"):
            (hot / name).mkdir()
        self.maintain()
        self.assertTrue((hot / "s-1-a-old").exists())
        self.assertTrue((hot / "s-2-b-new").exists())

    def test_low_free_space_can_trim_a_previous_snapshot_even_below_budget(self):
        hot = self.cache("concat-hot")
        self.keep(hot)
        for name in ("s-1-a-old", "s-2-b-new"):
            (hot / name).mkdir()
        self.policy["minimum_free_gib"] = 8
        with patch.object(dev.shutil, "disk_usage") as disk:
            disk.return_value.free = dev.GIB
            self.maintain()
        self.assertFalse((hot / "s-1-a-old").exists())
        self.assertTrue((hot / "s-2-b-new").exists())

    def test_symlinks_cannot_redirect_cleanup_into_other_files(self):
        outside = self.root / "project"
        outside.mkdir()
        document = outside / "concat.json"
        document.write_text("keep me")
        (self.incremental / "concat-outside").symlink_to(outside, target_is_directory=True)
        old = self.cache("concat_media-old", age=100)
        (old / "nested").symlink_to(outside, target_is_directory=True)
        modified = time.time() - 100 * 86400
        os.utime(old, (modified, modified))
        self.maintain()
        self.assertEqual(document.read_text(), "keep me")
        self.assertFalse(old.exists())
        self.assertTrue((self.incremental / "concat-outside").is_symlink())

    def test_redirected_incremental_root_is_refused(self):
        self.incremental.rmdir()
        outside = self.root / "media"
        outside.mkdir()
        self.incremental.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "redirected build cache"):
            self.maintain()

    def test_running_cargo_prevents_any_deletion(self):
        old = self.cache("concat_media-old", age=100)
        with dev.cargo_lock(self.profile) as held:
            self.assertTrue(held)
            self.assertIn("cleanup skipped", self.maintain())
        self.assertTrue(old.exists())
        self.maintain()
        self.assertFalse(old.exists())

    def test_shared_hardlinks_are_recounted_after_removing_a_variant(self):
        old = self.cache("concat_media-old", age=2)
        recent = self.cache("concat_media-new", age=1)
        (recent / "query-cache.bin").unlink()
        os.link(old / "query-cache.bin", recent / "query-cache.bin")
        self.policy["incremental_budget_gib"] = 1 / dev.GIB
        self.maintain()
        self.assertFalse(old.exists() or recent.exists())

    def test_oversized_working_cache_is_retained_and_reported(self):
        hot = self.cache("concat-hot")
        self.keep(hot)
        self.policy["incremental_budget_gib"] = 0
        self.assertIn("exceeds the budget", self.maintain())
        self.assertTrue(hot.exists())

    def test_read_only_or_redirected_marker_cannot_overwrite_a_project(self):
        document = self.root / "concat.json"
        document.write_text("keep me")
        (self.profile / ".capcon-hot-cache.json").symlink_to(document)
        self.cache("concat-hot")
        with self.assertRaisesRegex(RuntimeError, "redirected cache marker"):
            self.maintain(remember=True)
        self.assertEqual(document.read_text(), "keep me")

    def test_only_checks_and_tests_disable_ui_incremental_caching(self):
        for mode in ("build", "run", "test", "check"):
            command = dev.command(mode, [], "cargo")
            self.assertIn("quick", command)
            self.assertEqual("profile.quick.package.concat.incremental=false" in command,
                             mode in ("test", "check"))
        command = dev.command("test", ["-p", "concat-speech", "--lib"], "cargo")
        self.assertNotIn("concat", command)
        self.assertIn("concat-speech", command)

    def test_low_disk_stops_before_spawning_cargo(self):
        config = self.root / ".cargo"
        config.mkdir()
        (config / "dev-cache.toml").write_text("incremental_budget_gib=8\nunused_days=14\nminimum_free_gib=8\n")
        with patch.object(dev, "ROOT", self.root), patch.object(dev, "maintain"), \
             patch.object(dev.shutil, "disk_usage") as disk, patch.object(dev.subprocess, "run") as run:
            disk.return_value.free = 4 * dev.GIB
            with self.assertRaisesRegex(RuntimeError, "need 8 GiB"):
                dev.main(["build"])
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
