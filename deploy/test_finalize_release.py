import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("calc_finalize_release", Path(__file__).with_name("finalize-release.py"))
finalizer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(finalizer)


class ReleaseManifestTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.release = Path(self.temporary.name) / "release"
        for name in finalizer.REQUIRED_FILES:
            self.write(name, b"fixture")
        for name in ("bin/calc", "bin/calc-api", "bin/calc-preflight"):
            self.write(name, b"\x7fELFfixture")
            (self.release / name).chmod(0o700)
        self.write("web/.next/static/app.js", b"static")
        self.write("web/public/icon.svg", b"<svg/>")
        self.write("web/node_modules/lib/index.js", b"dependency")

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, name, data):
        path = self.release / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def test_complete_inventory_and_service_readable_permissions(self):
        (self.release / "web").chmod(0o700)
        (self.release / "web/server.js").chmod(0o600)
        (self.release / "web/node_modules/alias").symlink_to("lib")
        result = finalizer.finalize(self.release)
        for name in ("web/server.js", "web/.next/static/app.js", "web/public/icon.svg", "web/node_modules/lib/index.js", "build.txt"):
            self.assertEqual(result["files"][name], finalizer.digest(self.release / name))
        self.assertEqual(result["symlinks"], {"web/node_modules/alias": "lib"})
        self.assertNotIn("SHA256SUMS", result["files"])
        self.assertNotIn("release-manifest.json", result["files"])
        self.assertEqual((self.release / "web").stat().st_mode & 0o777, 0o755)
        self.assertEqual((self.release / "web/server.js").stat().st_mode & 0o777, 0o644)
        self.assertEqual((self.release / "bin/calc-api").stat().st_mode & 0o777, 0o755)
        self.assertEqual(json.loads((self.release / "release-manifest.json").read_text()), result)
        self.assertIn("  web/.next/static/app.js\n", (self.release / "SHA256SUMS").read_text())

    def test_refuses_api_only_staging(self):
        (self.release / "web/server.js").unlink()
        with self.assertRaisesRegex(ValueError, "Missing regular release file"):
            finalizer.finalize(self.release)

    def test_refuses_empty_static_directory(self):
        (self.release / "web/.next/static/app.js").unlink()
        with self.assertRaisesRegex(ValueError, "Missing release assets"):
            finalizer.finalize(self.release)

    def test_refuses_hidden_environment(self):
        self.write("web/.env.production", b"secret-value-not-for-output")
        with self.assertRaisesRegex(ValueError, "Environment file") as failure:
            finalizer.finalize(self.release)
        self.assertNotIn("secret-value-not-for-output", str(failure.exception))

    def test_refuses_absolute_escaping_and_broken_links(self):
        link = self.release / "web/node_modules/unsafe"
        for target in ("/tmp", "../../../..", "missing"):
            with self.subTest(target=target):
                link.symlink_to(target)
                with self.assertRaises((ValueError, FileNotFoundError)):
                    finalizer.finalize(self.release)
                link.unlink()

    def test_omits_build_cache_from_prebuilt_staging_and_manifest(self):
        self.write("web/.next/cache/nested/cache.bin", b"build cache")
        result = finalizer.finalize(self.release)
        self.assertFalse((self.release / "web/.next/cache").exists())
        self.assertFalse(any(name.startswith("web/.next/cache") for name in result["files"]))

    def test_removes_cache_link_without_touching_its_target(self):
        outside = Path(self.temporary.name) / "runtime-cache"
        outside.mkdir()
        (outside / "keep").write_text("runtime")
        cache = self.release / "web/.next/cache"
        cache.symlink_to(outside)
        result = finalizer.finalize(self.release)
        self.assertFalse(cache.is_symlink())
        self.assertEqual((outside / "keep").read_text(), "runtime")
        self.assertNotIn("web/.next/cache", result["symlinks"])

    def test_cache_omission_cannot_hide_an_unsafe_sibling(self):
        (self.release / "web/.next/cache-other").symlink_to("/var/cache/silicon-calc-web")
        with self.assertRaisesRegex(ValueError, "Absolute symlink"):
            finalizer.finalize(self.release)

    def test_cache_cleanup_does_not_follow_a_web_ancestor_symlink(self):
        original = self.release / "web/.next"
        outside = Path(self.temporary.name) / "external-next"
        original.rename(outside)
        (outside / "cache").mkdir()
        (outside / "cache/keep").write_text("runtime")
        original.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "must not be symlinks"):
            finalizer.finalize(self.release)
        self.assertEqual((outside / "cache/keep").read_text(), "runtime")


if __name__ == "__main__":
    unittest.main()
