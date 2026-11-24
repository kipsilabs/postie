import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import set_version

REPO = Path(__file__).resolve().parent.parent


class ParseTagTests(unittest.TestCase):
    def test_stable_tag(self):
        self.assertEqual(set_version.parse_tag("v0.1.1"), ("0.1.1", "0.1.1"))

    def test_prerelease_keeps_the_suffix_for_crates_but_not_the_bundle(self):
        self.assertEqual(set_version.parse_tag("v0.1.2-rc.1"), ("0.1.2-rc.1", "0.1.2"))

    def test_rejects_non_release_tags(self):
        for bad in ("0.1.1", "v0.1", "v0.1.1.2", "main", "v0.1.1-", ""):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                set_version.parse_tag(bad)


class LockTests(unittest.TestCase):
    LOCK = (
        'version = 4\n\n'
        '[[package]]\nname = "postie-core"\nversion = "0.1.0"\ndependencies = [\n "serde",\n]\n\n'
        '[[package]]\nname = "serde"\nversion = "0.1.0"\n'
        'source = "registry+https://github.com/rust-lang/crates.io-index"\n\n'
        '[[package]]\nname = "postie-cli"\nversion = "0.1.0"\n'
    )

    def test_only_workspace_crates_change(self):
        out = set_version.set_lock_versions(self.LOCK, {"postie-core", "postie-cli", "serde"}, "0.2.0")
        self.assertIn('name = "postie-core"\nversion = "0.2.0"', out)
        self.assertIn('name = "postie-cli"\nversion = "0.2.0"', out)
        # same name as a crate but from a registry: untouched
        self.assertIn('name = "serde"\nversion = "0.1.0"', out)


class ManifestTests(unittest.TestCase):
    def test_only_the_package_version_changes(self):
        manifest = (
            '[package]\nname = "postie-cli"\nversion = "0.1.0"\nedition = "2021"\n\n'
            '[dependencies]\nfoo = { version = "0.1.0" }\n'
        )
        out, name = set_version.set_manifest_version(manifest, "0.3.0")
        self.assertEqual(name, "postie-cli")
        self.assertIn('[package]\nname = "postie-cli"\nversion = "0.3.0"', out)
        self.assertIn('foo = { version = "0.1.0" }', out)


class RepoTests(unittest.TestCase):
    """Runs against a copy of this repository's real manifests and lockfiles."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        for rel in [p.relative_to(REPO) for p in (REPO / "rust").glob("*/Cargo.toml")] + [
            Path("rust/Cargo.toml"), Path("rust/Cargo.lock"),
            Path("rust/desktop/Cargo.lock"), Path("rust/desktop/tauri.conf.json"),
        ]:
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / rel, self.root / rel)

    def test_stamps_every_crate_lockfile_and_bundle(self):
        changed = set_version.stamp(self.root, "v9.8.7-rc.2")
        self.assertTrue(changed)
        for manifest in (self.root / "rust").glob("*/Cargo.toml"):
            section = set_version.package_section(manifest.read_text())
            self.assertIn('version = "9.8.7-rc.2"', section, manifest)
        for lock in ("rust/Cargo.lock", "rust/desktop/Cargo.lock"):
            self.assertIn('name = "postie-core"\nversion = "9.8.7-rc.2"', (self.root / lock).read_text())
        self.assertIn('"version": "9.8.7"', (self.root / "rust/desktop/tauri.conf.json").read_text())

    def test_is_idempotent(self):
        set_version.stamp(self.root, "v1.2.3")
        self.assertEqual(set_version.stamp(self.root, "v1.2.3"), [])

    def test_cli_rejects_a_bad_tag(self):
        r = subprocess.run(
            [sys.executable, str(REPO / "scripts/set_version.py"), "latest", str(self.root)],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 1)
        self.assertIn("not a release tag", r.stderr)


if __name__ == "__main__":
    unittest.main()
