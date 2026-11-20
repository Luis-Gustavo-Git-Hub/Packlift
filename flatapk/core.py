"""Package detection, metadata, architecture checks and installation plans."""

import gzip
import hashlib
import lzma
import os
import platform
import re
import struct
import tarfile
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath


class FlatapkError(Exception):
    pass


@dataclass
class Package:
    path: Path
    kind: str
    name: str
    version: str = ""
    architecture: str = ""
    requires: list[str] = field(default_factory=list)
    identifier: str = ""
    icon: bytes = b""
    icon_suffix: str = ""
    release: str = ""
    signature: str = "unknown"
    checksum: str = ""
    product: str = ""
    fingerprint: tuple[int, ...] = ()
    verified_digest: str = ""
    description: str = ""
    terminal: bool = False


@dataclass(frozen=True)
class Plan:
    method: str
    manager: str = ""
    family: str = ""
    multilib: bool = False


def host_info(runner=None) -> tuple[set[str], str]:
    if runner is not None and runner.sandbox:
        release = runner.capture(["cat", "/etc/os-release"])
        machine = runner.capture(["uname", "-m"]).strip()
        if runner.run(["test", "-e", "/run/ostree-booted"], streaming=False,
                      check=False).returncode == 0:
            release += "\nFLATAPK_IMMUTABLE=yes"
    else:
        release = Path("/etc/os-release").read_text(encoding="utf-8")
        machine = platform.machine()
        if Path("/run/ostree-booted").exists():
            release += "\nFLATAPK_IMMUTABLE=yes"
    identifiers = set()
    for line in release.splitlines():
        key, _, value = line.partition("=")
        if key in {"ID", "ID_LIKE"}:
            identifiers.update(value.strip().strip('"' + chr(39)).lower().split())
        elif key == "FLATAPK_IMMUTABLE":
            identifiers.add("immutable")
    return identifiers, machine.lower()


def architecture_group(architecture: str) -> str:
    value = architecture.lower()
    if re.fullmatch(r"i[3-6]86|x86|win32", value):
        return "x86"
    if value in {"amd64", "x86_64", "win64", "x64"}:
        return "x86_64"
    if value in {"all", "noarch", "neutral", "any"}:
        return "any"
    return value


def plan_for(package: Package, host: tuple[set[str], str]) -> Plan:
    distro, machine = host
    arch = architecture_group(package.architecture)
    machine = architecture_group(machine)
    if machine not in {"x86", "x86_64"}:
        raise FlatapkError("This host architecture is outside the supported x86 family.")
    if arch and arch not in {"any", machine} and (machine, arch) != ("x86_64", "x86"):
        raise FlatapkError("The package architecture is incompatible with this computer.")
    multilib = arch == "x86" and machine == "x86_64"
    if package.kind == "deb":
        native = bool(distro & {"debian", "ubuntu"}) and "immutable" not in distro
        return Plan("native" if native else "toolbox", "apt", "ubuntu", multilib)
    if package.kind == "rpm":
        if machine == "x86":
            raise FlatapkError("Current Fedora and openSUSE toolchains require a 64-bit host.")
        if distro & {"opensuse", "opensuse-leap", "opensuse-tumbleweed", "sles", "suse"} and "immutable" not in distro:
            return Plan("native", "zypper", multilib=multilib)
        native = bool(distro & {"fedora", "rhel", "centos", "rocky", "almalinux"}) and "immutable" not in distro
        return Plan("native" if native else "toolbox", "dnf", "fedora", multilib)
    if package.kind == "arch":
        if machine == "x86":
            raise FlatapkError("The supported Arch image requires a 64-bit host.")
        return Plan("native" if "arch" in distro else "toolbox", "pacman", "arch")
    if package.kind in {"exe", "msi", "setup"}:
        return Plan("wine", multilib=multilib)
    return Plan("local", multilib=multilib)


def pe_architecture(path: Path, header: bytes) -> str:
    with path.open("rb") as stream:
        offset = struct.unpack_from("<I", header, 60)[0]
        stream.seek(offset)
        pe = stream.read(6)
    if len(pe) != 6 or pe[:4] != b"PE\0\0":
        raise FlatapkError("This file does not contain a valid Windows PE header.")
    machine = struct.unpack_from("<H", pe, 4)[0]
    return {0x14C: "win32", 0x8664: "win64", 0xAA64: "arm64"}.get(machine, f"PE-{machine:04x}")


def tar_header(header: bytes) -> bool:
    if len(header) < 512:
        return False
    try:
        checksum = int(header[148:156].strip(b"\0 ") or b"0", 8)
    except ValueError:
        return False
    return checksum > 0 and checksum == sum(header[:148] + b" " * 8 + header[156:512])


def detect(path: Path) -> tuple[str, str]:
    with path.open("rb") as stream:
        header = stream.read(512)
    suffix = path.suffix.lower()
    if header.startswith(b"!<arch>\n") and header[8:24].rstrip(b" /") == b"debian-binary":
        return "deb", ""
    if header.startswith(b"\xed\xab\xee\xdb"):
        return "rpm", ""
    if header.startswith(b"\x7fELF") and header[8:11] in {b"AI\x01", b"AI\x02"}:
        return "appimage", elf_architecture(header)
    if header.startswith(b"MZ") and len(header) >= 64:
        return "setup" if suffix == ".setup" else "exe", pe_architecture(path, header)
    if header.startswith(bytes.fromhex("d0cf11e0a1b11ae1")):
        return "msi", ""
    if header.startswith(b"\x28\xb5\x2f\xfd") and path.name.lower().endswith(".pkg.tar.zst"):
        return "arch", ""
    for magic, opener in ((b"\x1f\x8b", gzip.open), (b"\xfd7zXZ\x00", lzma.open)):
        if header.startswith(magic):
            with opener(path, "rb") as stream:
                if tar_header(stream.read(512)):
                    return "tar", ""
            break
    raise FlatapkError("The file header does not match a supported package format.")


def elf_architecture(header: bytes) -> str:
    if len(header) < 20 or header[:4] != b"\x7fELF":
        return ""
    order = "little" if header[5] == 1 else "big"
    machine = int.from_bytes(header[18:20], order)
    return {3: "x86", 62: "x86_64", 183: "arm64", 40: "arm"}.get(machine, f"ELF-{machine}")


def control_fields(text: str, separator: str = ":") -> dict[str, str]:
    result: dict[str, str] = {}
    key = ""
    for line in text.splitlines():
        if not line.strip():
            break
        if line[:1].isspace() and key:
            result[key] += " " + line.strip()
        elif separator in line:
            key, value = line.split(separator, 1)
            key = key.strip()
            result[key] = value.strip()
    return result


def rpm_header(stream):
    introduction = stream.read(16)
    if len(introduction) != 16 or introduction[:4] != b"\x8e\xad\xe8\x01":
        raise FlatapkError("The RPM header is damaged.")
    count, size = struct.unpack(">II", introduction[8:])
    if count > 65536 or size > 32 * 1024 * 1024:
        raise FlatapkError("The RPM metadata is too large.")
    index = stream.read(count * 16)
    data = stream.read(size)
    if len(index) != count * 16 or len(data) != size:
        raise FlatapkError("The RPM metadata is incomplete.")
    return {tag: (kind, offset, length) for tag, kind, offset, length in
            struct.iter_unpack(">IIII", index)}, data


def rpm_metadata(package: Package):
    with package.path.open("rb") as stream:
        stream.seek(96)
        rpm_header(stream)
        stream.seek((-stream.tell()) % 8, os.SEEK_CUR)
        tags, data = rpm_header(stream)

    def strings(tag: int) -> list[str]:
        kind, offset, length = tags.get(tag, (0, 0, 0))
        if kind not in {6, 8, 9} or offset >= len(data) or length > 65536:
            return []
        values = data[offset:].split(b"\0", length)[:1 if kind in {6, 9} else length]
        return [value.decode("utf-8", "replace") for value in values if value]

    package.identifier = package.name = next(iter(strings(1000)), "")
    package.version = next(iter(strings(1001)), "")
    package.release = next(iter(strings(1002)), "")
    package.architecture = next(iter(strings(1022)), "")
    package.requires = strings(1049)


def inspect(path: Path, runner) -> Package:
    path = path.resolve(strict=True)
    if not path.is_file():
        raise FlatapkError("Choose a regular package file.")
    initial = fingerprint(path)
    kind, arch = detect(path)
    if not arch and kind in {"appimage", "exe", "setup"}:
        raise FlatapkError("The program contains an unknown architecture.")
    package = Package(path, kind, path.stem, architecture=arch)
    if kind == "deb":
        from .metadata import deb_control

        fields = control_fields(deb_control(runner, path))
        package.identifier = package.name = fields.get("Package", "")
        package.version = fields.get("Version", "")
        package.architecture = fields.get("Architecture", "")
        package.requires = [r.strip() for key in ("Pre-Depends", "Depends")
                            for r in fields.get(key, "").split(",") if r.strip()]
    elif kind == "rpm":
        rpm_metadata(package)
        package.version = "-".join(filter(None, (package.version, package.release)))
    elif kind == "arch":
        metadata = runner.capture(["bsdtar", "-xOf", str(path), ".PKGINFO"], local=True)
        fields = control_fields(metadata, "=")
        package.identifier = package.name = fields.get("pkgname", "")
        package.version = fields.get("pkgver", "")
        package.architecture = fields.get("arch", "")
        package.requires = [line.partition("=")[2].strip() for line in metadata.splitlines()
                            if line.startswith("depend =")]
    elif kind == "msi":
        table = runner.capture(["msiinfo", "export", str(path), "Property"], local=True)
        properties = dict(line.split("\t", 1) for line in table.splitlines() if "\t" in line)
        package.name = properties.get("ProductName", package.name)
        package.version = properties.get("ProductVersion", "")
        package.product = properties.get("ProductCode", "")
        summary = runner.capture(["msiinfo", "suminfo", str(path)], local=True)
        template = re.search(r"^(?:Template|Architecture):\s*(\w+)", summary, re.M | re.I)
        if template:
            package.architecture = {"Intel": "win32", "x64": "win64"}.get(
                template[1], template[1])
    elif kind == "tar":
        package.name = re.sub(r"\.tar\.(?:gz|xz)$", "", path.name, flags=re.I)
        with tarfile.open(path, "r|*") as archive:
            for index, member in enumerate(archive):
                if index >= 2048 or archive.offset > 32 * 1024 * 1024:
                    break
                if member.isfile() and member.mode & 0o111:
                    with archive.extractfile(member) as stream:
                        candidate = elf_architecture(stream.read(20))
                    if candidate:
                        package.architecture = candidate
                        break
    if kind in {"deb", "rpm", "arch"} and not (
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+_-]*", package.identifier)
        and package.version and package.architecture
    ):
        raise FlatapkError("The package is missing required metadata.")
    if kind == "rpm":
        result = runner.try_capture(["rpmkeys", "--checksig", "--verbose", str(path)])
        if result and re.search(r"Signature.*: OK", result, re.I):
            package.signature = "verified"
    signature = next((Path(f"{path}{suffix}") for suffix in (".sig", ".asc")
                      if Path(f"{path}{suffix}").is_file()), None)
    keyring = runner.data / "trustedkeys.gpg"
    if signature and keyring.is_file():
        runner.capture(["gpgv", "--keyring", str(keyring), str(signature), str(path)])
        package.signature = "verified"
    sidecar = Path(f"{path}.sha256")
    if sidecar.is_file():
        with sidecar.open(encoding="utf-8") as stream:
            digest = stream.read(4096).split()
        if not digest or not re.fullmatch(r"[0-9A-Fa-f]{64}", digest[0]):
            raise FlatapkError("The SHA-256 sidecar is invalid.")
        package.checksum = digest[0].lower()
    if package.signature == "verified":
        with path.open("rb") as stream:
            package.verified_digest = hashlib.file_digest(stream, "sha256").hexdigest()
    package.fingerprint = fingerprint(path)
    if initial != package.fingerprint:
        raise FlatapkError("The source file changed while it was being analyzed. Open it again.")
    return package


def fingerprint(path):
    stat = path.stat()
    return stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns


def relative_member(name: str) -> Path:
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts:
        raise FlatapkError("The archive contains an unsafe path.")
    return Path(*path.parts)
