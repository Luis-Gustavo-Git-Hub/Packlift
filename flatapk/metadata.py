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


def appimage_assets(package, runner):
    offset = appimage_offset(package.path)
    if not offset:
        return
    command = ["unsquashfs", "-o", str(offset)]
    path = str(package.path)
    extensions = (".png", ".svg", ".xpm")
    patterns = ["*.desktop", ".DirIcon", *(f"*{suffix}" for suffix in extensions),
                "usr/share/applications", "usr/share/icons", "usr/share/pixmaps"]
    listing = bounded_output(runner, [*command, "-ls", path, *patterns], 1024 * 1024)
    files = [line.removeprefix("squashfs-root/") for line in
             listing.decode("utf-8", "replace").splitlines() if line.startswith("squashfs-root/")]

    def read(name, limit):
        return bounded_output(runner, [*command, "-no-wildcards", "-cat", path, name], limit)

    desktops = sorted((name for name in files if name.endswith(".desktop") and (
        "/" not in name or name.startswith("usr/share/applications/"))),
        key=lambda name: ("/" in name, name))
    icon = ""
    for desktop in desktops[:8]:
        details = desktop_details(read(desktop, 64 * 1024).decode("utf-8", "replace"))
        if details:
            package.name, _, icon, package.terminal, package.description = details
            break

    icon_path = icon.lstrip("/")
    icon_name = Path(icon_path).name
    candidates = [name for name in files if Path(name).suffix.lower() in extensions and (
        Path(name).name == icon_name or Path(name).stem == icon_name)]
    candidates.sort(key=lambda name: (name != icon_path, "/" not in name, name))
    if ".DirIcon" in files:
        candidates.append(".DirIcon")
    for candidate in candidates[:12]:
        data = read(candidate, ICON_LIMIT)
        if data.startswith(b"\x89PNG\r\n\x1a\n"):
            suffix = ".png"
        elif re.search(rb"<svg(?:\s|>)", data[:8192]):
            suffix = ".svg"
        elif data.lstrip().startswith(b"/* XPM */"):
            suffix = ".xpm"
        else:
            continue
        package.icon, package.icon_suffix = data, suffix
        break


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


def pe_icon(path):
    """Return the largest embedded Windows application icon as a single-image ICO."""
    try:
        with path.open("rb") as stream:
            header = stream.read(64)
            if len(header) != 64:
                return b""
            stream.seek(struct.unpack_from("<I", header, 60)[0])
            coff = stream.read(24)
            if not coff.startswith(b"PE\0\0"):
                return b""
            sections = struct.unpack_from("<H", coff, 6)[0]
            optional_size = struct.unpack_from("<H", coff, 20)[0]
            optional = stream.read(optional_size)
            magic = struct.unpack_from("<H", optional)[0]
            data_dir = 96 if magic == 0x10B else 112 if magic == 0x20B else 0
            if not data_dir or len(optional) < data_dir + 24:
                return b""
            resource_rva, resource_size = struct.unpack_from("<II", optional, data_dir + 16)
            section_table = stream.tell()
            ranges = []
            for _ in range(min(sections, 96)):
                section = stream.read(40)
                if len(section) < 40:
                    break
                virtual_size, virtual_address, raw_size, raw_offset = struct.unpack_from("<IIII", section, 8)
                ranges.append((virtual_address, max(virtual_size, raw_size), raw_size, raw_offset))

            def raw_offset(rva):
                for address, span, size, offset in ranges:
                    if address <= rva < address + span and rva - address < size:
                        return offset + rva - address
                return None

            root_file = raw_offset(resource_rva)
            if root_file is None or resource_size < 16 or resource_size > 32 * 1024 * 1024:
                return b""
            stream.seek(root_file)
            resource = stream.read(resource_size)
    except (OSError, struct.error):
        return b""

    def directory(offset):
        if offset < 0 or offset + 16 > len(resource):
            return {}
        named, ids = struct.unpack_from("<HH", resource, offset + 12)
        count = min(named + ids, 4096)
        if offset + 16 + count * 8 > len(resource):
            return {}
        entries = {}
        for index in range(count):
            name, target = struct.unpack_from("<II", resource, offset + 16 + index * 8)
            if name & 0x80000000:
                continue
            entries[name & 0x7FFFFFFF] = target
        return entries

    def payload(target):
        entry = target & 0x7FFFFFFF
        if target & 0x80000000 or entry + 16 > len(resource):
            return b""
        rva, size = struct.unpack_from("<II", resource, entry)
        offset = raw_offset(rva)
        if offset is None or not 0 < size <= 2 * 1024 * 1024:
            return b""
        stream_data = path.open("rb")
        try:
            stream_data.seek(offset)
            return stream_data.read(size)
        finally:
            stream_data.close()

    def language_data(target):
        languages = directory(target & 0x7FFFFFFF) if target & 0x80000000 else {}
        return next((data for _, data_target in languages.items()
                     if (data := payload(data_target))), b"")

    root = directory(0)
    group_target = root.get(14)
    icons_target = root.get(3)
    if group_target is None or icons_target is None:
        return b""
    groups = directory(group_target & 0x7FFFFFFF) if group_target & 0x80000000 else {}
    icons = directory(icons_target & 0x7FFFFFFF) if icons_target & 0x80000000 else {}
    icon_data = {}
    for icon_id, target in icons.items():
        if target & 0x80000000:
            data = language_data(target)
            if data:
                icon_data[icon_id] = data
    candidates = []
    for target in groups.values():
        group = language_data(target) if target & 0x80000000 else payload(target)
        if len(group) < 6:
            continue
        reserved, kind, count = struct.unpack_from("<HHH", group)
        if reserved or kind != 1 or not 0 < count <= 64 or 6 + count * 14 > len(group):
            continue
        for index in range(count):
            width, height, colors, _, planes, depth, size, icon_id = struct.unpack_from(
                "<BBBBHHIH", group, 6 + index * 14)
            data = icon_data.get(icon_id, b"")
            if data and len(data) <= 2 * 1024 * 1024 and size <= len(data):
                candidates.append((width or 256, height or 256, depth, data))
    if not candidates:
        return b""
    width, height, _, data = max(candidates, key=lambda icon: (icon[0] * icon[1], icon[2]))
    entry = struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32,
                        len(data), 22)
    return struct.pack("<HHH", 0, 1, 1) + entry + data


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
        package.icon = pe_icon(package.path)
        package.icon_suffix = ".ico" if package.icon else ""
    elif package.kind == "appimage":
        appimage_assets(package, runner)
