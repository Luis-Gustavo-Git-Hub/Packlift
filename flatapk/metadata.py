"""Read display metadata from package payloads without running their contents."""

import re
import shutil
import shlex
import struct
import subprocess
import tarfile
from pathlib import Path

from .core import FlatapkError
from .linux import desktop_details, icon_from_paths

ICON_LIMIT = 2 * 1024 * 1024


def bounded_output(runner, argv, limit):
    command = [str(item) for item in argv]
    runner.write(f"$ {shlex.join(command)}")
    try:
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
            data = process.stdout.read(limit + 1)
            if len(data) > limit:
                process.kill()
                return b""
            return data if process.wait() == 0 else b""
    except FileNotFoundError:
        return b""


def deb_member(runner, path, component):
    names = runner.capture(["bsdtar", "-tf", str(path)], local=True).splitlines()
    return next((name for name in names if re.fullmatch(
        rf"{component}\.tar(?:\.(?:gz|xz|zst|bz2|lzma))?", name)), "")


def deb_archive_command(runner, path, component):
    member = deb_member(runner, path, component)
    if not member:
        raise FlatapkError("The Debian package is missing a required archive member.")
    return ["bsdtar", "-xOf", str(path), member]


def deb_control(runner, path):
    if not runner.sandbox and shutil.which("dpkg-deb"):
        return runner.capture(["dpkg-deb", "-f", str(path)], local=True)
    argv = deb_archive_command(runner, path, "control")
    with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as archive:
        try:
            output = subprocess.run(["bsdtar", "-xOf", "-", "./control", "control"],
                                    stdin=archive.stdout, stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL, check=False)
            if output.stdout:
                return output.stdout.decode("utf-8", "replace")
        finally:
            archive.stdout.close()
    raise FlatapkError("The Debian package does not contain readable control metadata.")


def deb_assets(package, runner):
    if not runner.sandbox and shutil.which("dpkg-deb"):
        argv = ["dpkg-deb", "--fsys-tarfile", str(package.path)]
        compression = False
    elif shutil.which("bsdtar"):
        argv = deb_archive_command(runner, package.path, "data")
        compression = True
    else:
        return {}
    icon_files, desktop_files = {}, {}
    # The reader accepts uncompressed data from dpkg or zstd frames from modern Debian packages.
    with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as payload:
        decoder = None
        try:
            stream = payload.stdout
            if compression:
                if argv[-1].endswith(".zst"):
                    decoder = subprocess.Popen(["zstd", "-dc"], stdin=stream, stdout=subprocess.PIPE,
                                               stderr=subprocess.DEVNULL)
                    stream = decoder.stdout
            with tarfile.open(fileobj=stream, mode="r|*") as archive:
                for index, member in enumerate(archive):
                    if index >= 1600 or archive.offset > 35 * 1024 * 1024:
                        break
                    if not member.isfile() or member.size > ICON_LIMIT:
                        continue
                    name = "/" + member.name.removeprefix("./").lstrip("/")
                    if name.endswith(".desktop") and "/applications/" in name and len(desktop_files) < 16:
                        desktop_files[name] = archive.extractfile(member).read()
                    if name.endswith((".png", ".svg", ".xpm")) and any(
                            folder in name for folder in ("/icons/", "/pixmaps/")) and len(icon_files) < 30:
                        icon_files[name] = archive.extractfile(member).read()
        finally:
            if decoder:
                decoder.stdout.close()
                decoder.terminate()
                decoder.wait()
            payload.stdout.close()
    return icon_files | desktop_files


def filesystem_asset(package, runner, listing, content):
    files = bounded_output(runner, listing, 2 * 1024 * 1024).decode("utf-8", "replace").splitlines()
    paths = {"/" + name.removeprefix("./").lstrip("/"): name for name in files}
    candidates = [path for path in paths if path.endswith(".desktop") and
                  ("/applications/" in path or path.count("/") == 1)]
    for path in candidates[:12]:
        details = desktop_details(bounded_output(runner, content(paths[path]), 64 * 1024).decode("utf-8", "replace"))
        if details:
            name, _, icon, _, _ = details
            package.name = name or package.name
            icon_path = icon_from_paths(paths, icon)
            if not icon_path:
                icon_path = next((p for p in paths if Path(p).name in
                                  {icon + ".png", icon + ".svg", icon}), None) if icon else None
            if icon_path:
                package.icon = bounded_output(runner, content(paths[icon_path]), ICON_LIMIT)
                package.icon_suffix = Path(icon_path).suffix
            break


def appimage_offset(path):
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[8:11] != b"AI\x02" or header[4] not in (1, 2):
            return 0
        byte_order = "<" if header[5] == 1 else ">"
        if header[4] == 2:
            offset = struct.unpack_from(byte_order + "Q", header, 40)[0]
            entry, count = struct.unpack_from(byte_order + "HH", header, 58)
        else:
            offset = struct.unpack_from(byte_order + "I", header, 32)[0]
            entry, count = struct.unpack_from(byte_order + "HH", header, 46)
        offset += entry * count
        if offset >= path.stat().st_size:
            return 0
        stream.seek(offset)
        if stream.read(4) == b"hsqs":
            return offset
    return 0


def pe_version(path):
    values = {}
    resource = b""
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) < 64:
            return values
        stream.seek(struct.unpack_from("<I", header, 60)[0])
        coff = stream.read(24)
        if len(coff) < 24 or not coff.startswith(b"PE\0\0"):
            return values
        sections = struct.unpack_from("<H", coff, 6)[0]
        optional_size = struct.unpack_from("<H", coff, 20)[0]
        stream.seek(optional_size, 1)
        for _ in range(min(sections, 96)):
            section = stream.read(40)
            if len(section) < 40:
                break
            if section[:8].rstrip(b"\0") == b".rsrc":
                size, offset = struct.unpack_from("<II", section, 16)
                stream.seek(offset)
                resource = stream.read(min(size, 16 * 1024 * 1024))
                break
    for key in ("ProductName", "FileDescription", "ProductVersion", "FileVersion"):
        encoded = (key + "\0").encode("utf-16-le")
        for match in re.finditer(re.escape(encoded), resource):
            start = match.start() - 6
            if start < 0:
                continue
            length, characters, kind = struct.unpack_from("<HHH", resource, start)
            value_at = (match.end() + 3) & ~3
            end = value_at + characters * 2
            if kind == 1 and 0 < characters < 256 and end <= min(start + length, len(resource)):
                value = resource[value_at:end].decode("utf-16le", errors="replace").strip("\0 ")
                if value and all(character.isprintable() for character in value):
                    values[key] = value
                    break
    return values


def decorate(package, runner):
    if package.kind == "deb":
        assets = deb_assets(package, runner)
        for path, data in assets.items():
            if not path.endswith(".desktop"):
                continue
            details = desktop_details(data.decode("utf-8", "replace"))
            if details:
                package.name, _, icon, _, _ = details
                selected = icon_from_paths(assets, icon)
                if selected:
                    package.icon = assets[selected]
                    package.icon_suffix = Path(selected).suffix
                break
    elif package.kind in {"rpm", "arch", "tar"}:
        path = str(package.path)
        filesystem_asset(package, runner, ["bsdtar", "-tf", path],
                         lambda name: ["bsdtar", "-xOf", path, "--", name])
    elif package.kind in {"exe", "setup"}:
        values = pe_version(package.path)
        package.name = values.get("ProductName") or values.get("FileDescription") or package.name
        package.version = values.get("ProductVersion") or values.get("FileVersion") or ""
    elif package.kind == "appimage":
        offset = appimage_offset(package.path)
        if offset:
            command = ["unsquashfs", "-o", str(offset)]
            path = str(package.path)
            listing = bounded_output(runner, [*command, "-ls", path], 1024 * 1024)
            files = [line.removeprefix("squashfs-root/") for line in
                     listing.decode("utf-8", "replace").splitlines()
                     if line.startswith("squashfs-root/")]
            desktops = [name for name in files if "/" not in name and name.endswith(".desktop")]
            for desktop in desktops[:8]:
                data = bounded_output(runner, [*command, "-cat", path, desktop], 64 * 1024)
                details = desktop_details(data.decode("utf-8", "replace"))
                if details:
                    package.name, _, icon, package.terminal, package.description = details
                    names = {icon, f"{icon}.png", f"{icon}.svg"}
                    chosen = next((name for name in files if Path(name).name in names), None)
                    if chosen:
                        package.icon = bounded_output(runner, [*command, "-cat", path, chosen], ICON_LIMIT)
                        package.icon_suffix = Path(chosen).suffix
                    break
