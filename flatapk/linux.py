"""Native, Toolbox, archive and AppImage installation backends."""

import configparser
import io
import os
import re
import shlex
import shutil
import stat
import tarfile
from pathlib import Path

from .core import FlatapkError, architecture_group, elf_architecture, relative_member

IMAGES = {
    "ubuntu": "quay.io/toolbx/ubuntu-toolbox:24.04",
    "fedora": "registry.fedoraproject.org/fedora-toolbox:43",
    "arch": "quay.io/toolbx/arch-toolbox:latest",
}

MIME_SUFFIXES = {
    "deb": ".deb", "rpm": ".rpm", "arch": ".pkg.tar.zst", "appimage": ".AppImage",
    "msi": ".msi", "exe": ".exe", "setup": ".exe", "tar": ".tar",
}


def toolbox(runner, family, registry):
    if not runner.available("toolbox") or not runner.available("podman"):
        raise FlatapkError("Install Toolbox and Podman on the host before continuing.")
    name = f"flatapk-{family}"
    exists = runner.run(["podman", "container", "exists", name], check=False).returncode
    if exists == 0:
        if name not in registry.containers():
            raise FlatapkError(f"Toolbox container {name} already exists outside Flatapk.")
        return name
    if exists != 1:
        raise FlatapkError("Podman could not check the Toolbox environment.")
    image = os.environ.get(f"FLATAPK_{family.upper()}_IMAGE", IMAGES[family])
    runner.run(["podman", "pull", image])
    created = runner.run(["toolbox", "create", "--container", name, "--image", image],
                         check=False)
    if created.returncode:
        raise FlatapkError(f"Toolbox could not create {name} from {image}. Check the log.")
    registry.add_container(name)
    return name


def invoke(container):
    return ["toolbox", "run", "--container", container] if container else []


def elevated(runner, container, arguments):
    if not container and not runner.available("pkexec"):
        raise FlatapkError("Install polkit and start a desktop authentication agent first.")
    runner.run([*invoke(container), "sudo" if container else "pkexec", *arguments])


def package_target(package, manager):
    if manager == "apt" and package.architecture != "all":
        return f"{package.identifier}:{package.architecture}"
    if manager in {"dnf", "zypper"}:
        return f"{package.identifier}.{package.architecture}"
    return package.identifier


def package_files(runner, manager, target, container):
    commands = {
        "apt": ["dpkg-query", "-L", target],
        "dnf": ["rpm", "-ql", target],
        "zypper": ["rpm", "-ql", target],
        "pacman": ["pacman", "-Qlq", target],
    }
    return runner.capture([*invoke(container), *commands[manager]]).splitlines()


def already_installed(runner, manager, target, container):
    commands = {
        "apt": ["dpkg-query", "-W", "-f=${Status}", target],
        "dnf": ["rpm", "-q", target],
        "zypper": ["rpm", "-q", target],
        "pacman": ["pacman", "-Q", target],
    }
    result = runner.run([*invoke(container), *commands[manager]], streaming=False, check=False)
    if result.returncode:
        return False
    return manager != "apt" or result.stdout.split()[-1:] == ["installed"]


def install_system(runner, package, plan, source, container, step):
    manager = plan.manager
    if plan.multilib and manager == "apt":
        foreign = runner.capture([*invoke(container), "dpkg", "--print-foreign-architectures"])
        if "i386" not in foreign.split():
            elevated(runner, container, ["dpkg", "--add-architecture", "i386"])
    step("dependencies")
    if container and manager == "apt":
        elevated(runner, container, ["apt-get", "update"])
    if not container and manager == "apt" and plan.multilib:
        elevated(runner, container, ["apt-get", "update"])
    if manager == "apt":
        elevated(runner, container, ["apt-get", "install", "--download-only", "-y", source])
    elif manager == "dnf":
        elevated(runner, container, ["dnf", "install", "--downloadonly", "-y", source])
    step("install")
    commands = {
        "apt": ["apt-get", "install", "-y", source],
        "dnf": ["dnf", "install", "-y", source],
        "zypper": ["zypper", "--non-interactive", "install", source],
        "pacman": ["pacman", "-U", "--noconfirm", "--", source],
    }
    elevated(runner, container, commands[manager])


def remove_system(runner, manager, target, container):
    commands = {
        "apt": ["apt-get", "remove", "-y", "--", target],
        "dnf": ["dnf", "remove", "-y", "--", target],
        "zypper": ["zypper", "--non-interactive", "remove", "--", target],
        "pacman": ["pacman", "-R", "--noconfirm", "--", target],
    }
    elevated(runner, container, commands[manager])


def desktop_details(contents):
    desktop = configparser.ConfigParser(interpolation=None, strict=False)
    try:
        desktop.read_file(io.StringIO(contents))
        entry = desktop["Desktop Entry"]
        if entry.get("Type", "Application") != "Application":
            return None
        argv = shlex.split(entry.get("Exec", ""))
    except (ValueError, KeyError, configparser.Error):
        return None
    argv = [arg.replace("%%", "%") for arg in argv if not re.search(r"%[fFuUdDnNickvm]", arg)]
    if not argv or entry.get("NoDisplay", "").lower() == "true":
        return None
    return entry.get("Name", argv[0]), argv, entry.get("Icon", ""), entry.get("Terminal", "").lower() == "true"


def icon_from_paths(files, icon):
    if icon in files and Path(icon).suffix in {".png", ".svg", ".xpm"}:
        return icon
    normalized = Path(icon).name
    if not normalized:
        return None
    names = {normalized, *(normalized + ext for ext in (".svg", ".png", ".xpm"))}
    candidates = [p for p in files if Path(p).name in names
                  and any(folder in p for folder in ("/icons/", "/pixmaps/"))]
    return min(candidates, key=lambda p: (p != icon, "scalable" not in p, "128x128" not in p,
                                           "64x64" not in p, p), default=None)


def system_launchers(runner, package, plan, target, container, directory):
    files = package_files(runner, plan.manager, target, container)
    launchers = []
    icon_directory = directory / "icons"
    for path in files:
        if not path.startswith(("/usr/share/applications/", "/opt/")) or not path.endswith(".desktop"):
            continue
        contents = runner.try_capture([*invoke(container), "cat", "--", path])
        details = desktop_details(contents)
        if not details:
            continue
        name, argv, icon, terminal = details
        candidate = icon_from_paths(files, icon)
        if candidate and container:
            icon_directory.mkdir(exist_ok=True)
            exported = icon_directory / f"{len(launchers)}{Path(candidate).suffix}"
            runner.copy_stdout([*invoke(container), "cat", "--", candidate], exported)
            icon = str(exported)
        launchers.append((name, [*invoke(container), *argv], icon, terminal))
    if not launchers:
        executable = next((p for p in files if Path(p).parent in {
            Path("/usr/bin"), Path("/usr/local/bin"), Path("/bin")}), None)
        if not executable:
            raise FlatapkError("The package did not provide an application launcher or executable.")
        launchers.append((package.name, [*invoke(container), executable], "", True))
    return launchers


def unpack(archive: Path, directory: Path):
    executables = []
    desktop_files = []
    with tarfile.open(archive) as stream:
        for member in stream:
            relative = relative_member(member.name)
            if not relative.parts:
                if member.isdir():
                    continue
                raise FlatapkError("The archive contains an invalid file name.")
            target = directory / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                raise FlatapkError("The archive contains a link or special file. Use an AppImage or a system package.")
            target.parent.mkdir(parents=True, exist_ok=True)
            with stream.extractfile(member) as source, target.open("xb") as destination:
                shutil.copyfileobj(source, destination)
            target.chmod(stat.S_IRUSR | stat.S_IWUSR | (stat.S_IXUSR if member.mode & 0o111 else 0))
            if member.mode & 0o111:
                with target.open("rb") as source:
                    head = source.read(20)
                if not target.name.endswith((".so", ".dll")) and (head.startswith(b"#!") or elf_architecture(head)):
                    executables.append(target)
            if target.suffix == ".desktop":
                desktop_files.append(target)
    return executables, desktop_files


def archive_launcher(package, directory, targets, desktop_files, select, machine):
    host_arch = architecture_group(machine)
    compatible = []
    for target in targets:
        with target.open("rb") as stream:
            arch = elf_architecture(stream.read(20))
        if not arch or arch == host_arch or (host_arch, arch) == ("x86_64", "x86"):
            compatible.append(target)
    if not compatible:
        raise FlatapkError("The archive has no executable compatible with this computer.")
    for desktop in desktop_files:
        details = desktop_details(desktop.read_text(encoding="utf-8", errors="replace"))
        if details:
            name, argv, icon, terminal = details
            matches = [p for p in compatible if p.name == Path(argv[0]).name]
            if len(matches) == 1:
                icons = [str(p) for p in directory.rglob("*") if p.is_file()]
                return name, ["env", "-C", str(matches[0].parent), str(matches[0]), *argv[1:]], icon_from_paths(icons, icon) or "", terminal
    ordered = sorted(compatible, key=lambda p: (p.name not in {package.name, "AppRun"},
                                                len(p.relative_to(directory).parts), p.name))
    chosen = ordered[0] if len(ordered) == 1 else select(ordered, directory)
    if chosen not in ordered:
        raise FlatapkError("No executable was selected. The archive was not installed.")
    return package.name, ["env", "-C", str(chosen.parent), str(chosen)], "", False


def install_local(package, source, directory, select, machine):
    if package.kind == "appimage":
        target = directory / "program.AppImage"
        shutil.copyfile(source, target)
        target.chmod(0o700)
        with target.open("rb") as stream:
            type2 = stream.read(11)[8:11] == b"AI\x02"
        argv = ["env", "APPIMAGE_EXTRACT_AND_RUN=1", str(target)] if type2 else [str(target)]
        icon = ""
        if package.icon:
            icon = directory / f"icon{package.icon_suffix}"
            icon.write_bytes(package.icon)
        return [(package.name, argv, str(icon), False)]
    payload = directory / "payload"
    payload.mkdir()
    targets, desktops = unpack(source, payload)
    return [archive_launcher(package, payload, targets, desktops, select, machine)]
