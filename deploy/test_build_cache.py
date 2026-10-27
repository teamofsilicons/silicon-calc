import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(shutil.which("cargo"), "Cargo is required for the cache regression")
class WorkspaceBuildCacheTests(unittest.TestCase):
    def test_normalized_source_mtime_rebuilds_local_crates_and_keeps_dependencies(self):
        with tempfile.TemporaryDirectory(prefix="calc-build-cache-") as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            target = root / "target"
            environment = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL="0")

            def write(name, text):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
                os.utime(path, (946684800, 946684800))
                return path

            write("workspace/Cargo.toml", '[workspace]\nresolver="2"\nmembers=["app", "local"]\n')
            write("workspace/app/Cargo.toml", '[package]\nname="cache-app"\nversion="0.1.0"\nedition="2021"\n[dependencies]\ncache-local={path="../local"}\n')
            write("workspace/app/src/main.rs", 'fn main() { println!("{}", cache_local::value()); }\n')
            write("workspace/local/Cargo.toml", '[package]\nname="cache-local"\nversion="0.1.0"\nedition="2021"\n[dependencies]\ncache-external={path="../../external"}\n')
            write("external/Cargo.toml", '[package]\nname="cache-external"\nversion="0.1.0"\nedition="2021"\n')
            write("external/src/lib.rs", 'pub fn value() -> u32 { 7 }\n')
            local_source = "workspace/local/src/lib.rs"
            write(local_source, 'pub fn value() -> u32 { cache_external::value() + 1 }\n')

            def build():
                subprocess.run(["cargo", "build", "--offline", "--release", "--quiet"],
                               cwd=workspace, env=environment, check=True, capture_output=True)

            def value():
                return subprocess.check_output([str(target / "release/cache-app")], text=True).strip()

            build()
            self.assertEqual(value(), "8")
            dependencies = list((target / "release/deps").glob("libcache_external-*"))
            self.assertTrue(dependencies)
            retained = {path: (path.read_bytes(), path.stat().st_mtime_ns) for path in dependencies}

            write(local_source, 'pub fn value() -> u32 { cache_external::value() + 2 }\n')
            build()
            self.assertEqual(value(), "8", "Fixture must reproduce Cargo's same-mtime stale result")

            subprocess.run([sys.executable, str(Path(__file__).with_name("clean-workspace-release.py")),
                            str(workspace / "Cargo.toml")], env=environment, check=True, capture_output=True)
            for path, state in retained.items():
                self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), state)
            self.assertFalse((target / "release/cache-app").exists())
            build()
            self.assertEqual(value(), "9")
            for path, state in retained.items():
                self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), state)


if __name__ == "__main__":
    unittest.main()
