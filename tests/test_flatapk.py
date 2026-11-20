import hashlib
import io
import shutil
import struct
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from flatapk.core import FlatapkError, Package, detect, inspect, plan_for
from flatapk.installer import Installer
from flatapk.system import Runner, desktop_exec, operation
from flatapk.wine import Wine, environment, windows_argv


class FlatapkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.installer = Installer(data=self.root / "state", applications=self.root / "applications")

    def archive(self, name="program.tar.gz", member="example/hello"):
        path = self.root / name
        contents = b"#!/bin/sh\nprintf 'flatapk-fixture\\n'\n"
        with tarfile.open(path, "w:gz") as archive:
            info = tarfile.TarInfo(member)
            info.mode = 0o755
            info.size = len(contents)
            archive.addfile(info, io.BytesIO(contents))
        return path

    def test_magic_ignores_extension_and_rejects_invalid_windows(self):
        package = self.archive("file.deb")
        self.assertEqual(detect(package), ("tar", ""))
        package.write_bytes(b"MZ" + b"\0" * 62)
        with self.assertRaises(FlatapkError):
            detect(package)

    def test_supported_plans_and_multilib(self):
        deb = Package(Path("/tmp/example"), "deb", "Example", architecture="i386")
        rpm = Package(Path("/tmp/example"), "rpm", "Example", architecture="x86_64")
        self.assertEqual(plan_for(deb, ({"ubuntu"}, "x86_64")).method, "native")
        self.assertEqual(plan_for(deb, ({"fedora"}, "x86_64")).family, "ubuntu")
        self.assertTrue(plan_for(deb, ({"fedora"}, "x86_64")).multilib)
        self.assertEqual(plan_for(rpm, ({"ubuntu"}, "x86_64")).family, "fedora")
        self.assertEqual(plan_for(rpm, ({"fedora"}, "x86_64")).method, "native")
        self.assertEqual(plan_for(rpm, ({"fedora", "immutable"}, "x86_64")).method, "toolbox")
        self.assertEqual(plan_for(rpm, ({"opensuse"}, "x86_64")).manager, "zypper")
        with self.assertRaises(FlatapkError):
            plan_for(rpm, ({"debian"}, "i686"))

    @unittest.skipUnless(shutil.which("dpkg-deb"), "requires dpkg-deb for the fixture")
    def test_reads_real_debian_metadata_and_desktop(self):
        tree = self.root / "deb"
        control = tree / "DEBIAN"
        control.mkdir(parents=True)
        control.chmod(0o755)
        (control / "control").write_text(
            "Package: flatapk-fixture\nVersion: 1.2\nArchitecture: all\n"
            "Maintainer: Test <test@example.invalid>\nDepends: bash (>= 1), coreutils\n"
            "Description: Flatapk test fixture\n", encoding="utf-8")
        desktop = tree / "usr/share/applications/fixture.desktop"
        desktop.parent.mkdir(parents=True)
        desktop.write_text("[Desktop Entry]\nType=Application\nName=Fixture program\n"
                           "Exec=fixture\nIcon=fixture\n", encoding="utf-8")
        icon = tree / "usr/share/pixmaps/fixture.svg"
        icon.parent.mkdir(parents=True)
        icon.write_bytes(b'<svg xmlns="http://www.w3.org/2000/svg"/>')
        package = self.root / "fixture"
        subprocess.run(["dpkg-deb", "-Zzstd", "--root-owner-group", "--build", str(tree), str(package)], check=True,
                       stdout=subprocess.DEVNULL)
        info, _ = self.installer.probe(package)
        self.assertEqual((info.identifier, info.name, info.version), ("flatapk-fixture", "Fixture program", "1.2"))
        self.assertEqual(info.requires, ["bash (>= 1)", "coreutils"])
        self.assertEqual(info.icon, icon.read_bytes())
        if shutil.which("bsdtar"):
            self.installer.runner.sandbox = True
            self.assertEqual(inspect(package, self.installer.runner).identifier, "flatapk-fixture")

    def test_pe_architecture(self):
        path = self.root / "installer.setup"
        for code, expected in ((0x14C, "win32"), (0x8664, "win64")):
            content = bytearray(72)
            content[:2] = b"MZ"
            struct.pack_into("<I", content, 60, 64)
            content[64:68] = b"PE\0\0"
            struct.pack_into("<H", content, 68, code)
            path.write_bytes(content)
            self.assertEqual(detect(path), ("setup", expected))

    def test_local_install_launch_and_uninstall(self):
        path = self.archive()
        Path(f"{path}.sha256").write_text(hashlib.sha256(path.read_bytes()).hexdigest())
        package, plan = self.installer.probe(path)
        stages = []
        app = self.installer.install(package, plan, stages.append, lambda *_: self.fail("unexpected prompt"))
        self.assertEqual(stages, ["analyze", "compatibility", "environment", "install", "shortcut", "done"])
        self.assertEqual(self.installer.registry.apps(), [app])
        export = app["exports"][0]
        self.assertIn("Exec=", Path(export["desktop"]).read_text())
        opened = subprocess.check_output(export["argv"], text=True).strip()
        self.assertEqual(opened, "flatapk-fixture")
        self.installer.uninstall(app, stages.append)
        self.assertEqual(self.installer.registry.apps(), [])
        self.assertFalse(Path(app["directory"]).exists())
        self.assertFalse(Path(export["desktop"]).exists())

    @unittest.skipUnless(
        all(shutil.which(tool) for tool in ("mksquashfs", "unsquashfs")),
        "requires squashfs-tools for the AppImage fixture",
    )
    def test_appimage_preserves_description_and_terminal_preference(self):
        payload = self.root / "payload"
        payload.mkdir()
        (payload / "fixture.desktop").write_text(
            "[Desktop Entry]\nType=Application\nName=Builder fixture\n"
            "Comment=Command-line tool\nExec=AppRun\nTerminal=true\n",
            encoding="utf-8",
        )
        filesystem = self.root / "payload.squashfs"
        subprocess.run(["mksquashfs", str(payload), str(filesystem), "-processors", "1"],
                       check=True, stdout=subprocess.DEVNULL)
        # Use a host ELF as the header for a real AppImage type 2 payload.
        runtime = bytearray(Path("/bin/true").read_bytes())
        runtime[8:11] = b"AI\x02"
        source = self.root / "fixture.AppImage"
        source.write_bytes(runtime + filesystem.read_bytes())

        package, plan = self.installer.probe(source)
        self.assertEqual((package.name, package.description), ("Builder fixture", "Command-line tool"))
        app = self.installer.install(package, plan, lambda _: None, lambda *_: None)
        export = app["exports"][0]
        self.assertTrue(export["terminal"])
        self.assertIn("Terminal=true\n", Path(export["desktop"]).read_text())

    def test_appimage_launch_reports_output_from_existing_shortcut(self):
        image = self.root / "program.AppImage"
        image.write_text("#!/bin/sh\nprintf 'Use the graphical launcher.\\n' >&2\nexit 1\n")
        image.chmod(0o700)
        app = {"exports": [{"argv": ["env", "APPIMAGE_EXTRACT_AND_RUN=1", str(image)]}]}
        with self.assertRaisesRegex(FlatapkError, "AppImage exited"):
            self.installer.launch_appimage(app)
        log = self.installer.runner.log.read_text()
        self.assertIn("Use the graphical launcher.", log)
        self.assertIn("AppImage exited with status 1.", log)

    def test_archive_traversal_rolls_back(self):
        package, plan = self.installer.probe(self.archive(member="../escape"))
        with self.assertRaisesRegex(FlatapkError, "unsafe path"):
            self.installer.install(package, plan, lambda _: None, lambda *_: None)
        self.assertEqual(self.installer.registry.apps(), [])
        self.assertEqual(list((self.installer.data / "installed").iterdir()), [])
        self.assertFalse((self.root / "escape").exists())

    def test_archive_accepts_internal_links_and_rejects_external_links(self):
        fixture = self.root / "linked.tar.xz"
        for target in ("../lib/library", "../../../outside"):
            with tarfile.open(fixture, "w:xz") as archive:
                for name, payload, permissions in (
                    ("program/bin/hello", b"#!/bin/sh\ncat link\n", 0o755),
                    ("program/lib/library", b"internal\n", 0o644),
                ):
                    member = tarfile.TarInfo(name)
                    member.size = len(payload)
                    member.mode = permissions
                    archive.addfile(member, io.BytesIO(payload))
                symbol = tarfile.TarInfo("program/bin/link")
                symbol.type = tarfile.SYMTYPE
                symbol.linkname = target
                archive.addfile(symbol)
            package, plan = self.installer.probe(fixture)
            if "outside" in target:
                with self.assertRaisesRegex(FlatapkError, "unsafe path"):
                    self.installer.install(package, plan, lambda _: None, lambda *_: None)
                continue
            app = self.installer.install(package, plan, lambda _: None, lambda *_: None)
            self.assertEqual(subprocess.check_output(app["exports"][0]["argv"], text=True), "internal\n")
            self.installer.uninstall(app, lambda _: None)

    def test_integrity_and_changed_source(self):
        path = self.archive()
        sidecar = Path(f"{path}.sha256")
        sidecar.write_text("0" * 64)
        package, plan = self.installer.probe(path)
        with self.assertRaisesRegex(FlatapkError, "SHA-256"):
            self.installer.install(package, plan, lambda _: None, lambda *_: None)
        sidecar.unlink()
        package, plan = self.installer.probe(path)
        self.archive(member="other")
        with self.assertRaisesRegex(FlatapkError, "changed"):
            self.installer.install(package, plan, lambda _: None, lambda *_: None)

    def test_lock_rejects_overlapping_operations(self):
        with operation(self.installer.data), self.assertRaisesRegex(FlatapkError, "operation"):
            with operation(self.installer.data):
                self.fail("lock was acquired a second time")

    def test_failed_wine_operation_restores_shared_prefix(self):
        wine = Wine(Runner(self.root / "state"), "wine", "x86_64")
        wine.prefix.mkdir(parents=True)
        original = wine.prefix / "system.reg"
        original.write_text("original")
        with patch.object(wine, "wait"), self.assertRaisesRegex(RuntimeError, "fixture"):
            with wine.backup(self.root / "snapshot"):
                original.write_text("changed")
                (wine.prefix / "new-file").touch()
                raise RuntimeError("fixture failed")
        self.assertEqual(original.read_text(), "original")
        self.assertFalse((wine.prefix / "new-file").exists())

    def test_wine_command_line_and_profile_validation(self):
        self.assertEqual(windows_argv(r'"C:\Program Files\Fixture\unins.exe" /S ""'),
                         [r"C:\Program Files\Fixture\unins.exe", "/S", ""])
        self.assertEqual(environment("WINEDLLOVERRIDES=foo=n,b\nLANG=pt_BR.UTF-8"),
                         {"WINEDLLOVERRIDES": "foo=n,b", "LANG": "pt_BR.UTF-8"})
        with self.assertRaises(FlatapkError):
            environment("WINEPREFIX=/tmp/other")

    def test_desktop_entry_escapes_field_codes(self):
        self.assertEqual(desktop_exec(["toolbox", "/path/a b", "100%"]), 'toolbox "/path/a b" 100%%')


if __name__ == "__main__":
    unittest.main()
