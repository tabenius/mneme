"""Packaging/installation contract tests: no Rust build, services or session data."""
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which("dpkg-deb"), "Debian packaging tools required")
class DistributionTests(unittest.TestCase):
    def test_package_and_user_install(self):
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            binary = temp / "mneme"
            binary.write_text("#!/bin/sh\necho 'mneme 0.2.0'\n")
            binary.chmod(0o755)
            out = temp / "dist"
            subprocess.run(
                ["bash", str(ROOT / "scripts/package-release.sh"), str(binary),
                 "x86_64-unknown-linux-musl", str(out)], check=True,
                env={k: v for k, v in os.environ.items() if k not in ("GITHUB_REF_TYPE", "GITHUB_REF_NAME")},
                capture_output=True,
            )
            package, = out.glob("*.deb")
            arch = subprocess.check_output(["dpkg-deb", "-f", str(package), "Architecture"], text=True)
            self.assertEqual(arch.strip(), "amd64")
            deb_root = temp / "deb"
            subprocess.run(["dpkg-deb", "-R", str(package), str(deb_root)], check=True)
            self.assertTrue((deb_root / "usr/bin/mneme").exists())
            self.assertIn("ExecStart=/usr/bin/mneme", (deb_root / "usr/lib/systemd/user/mneme-collector.service").read_text())
            self.assertEqual([p.name for p in (deb_root / "DEBIAN").iterdir()], ["control"])

            archive, = out.glob("*.tar.gz")
            extracted = temp / "extracted"
            with tarfile.open(archive) as tar:
                tar.extractall(extracted, filter="data")
            bundle, = extracted.iterdir()
            home = temp / "home"
            home.mkdir()
            subprocess.run(["bash", str(bundle / "install.sh")], env={**os.environ, "HOME": str(home)}, check=True, capture_output=True)
            installed = home / ".local/bin/mneme"
            self.assertTrue(installed.is_symlink())
            self.assertEqual(subprocess.check_output([str(installed), "--version"], text=True).strip(), "mneme 0.2.0")
            self.assertTrue((home / ".config/systemd/user/mneme-collector.service").exists())
            subprocess.run(["sha256sum", "--check", "SHA256SUMS-x86_64-unknown-linux-musl"], cwd=out, check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
