"""Windows installers through a shared Wine prefix and exported menu shortcuts."""

import hashlib
import os
import re
import shutil
import subprocess
from contextlib import contextmanager
from pathlib import Path


from .core import FlatapkError, architecture_group
from .linux import desktop_details, icon_from_paths

FLATPAK_WINE = "org.winehq.Wine//wow64-25.08"
PROTON = "proton"
FLATHUB_REPOSITORY = "https://dl.flathub.org/repo/flathub.flatpakrepo"
UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
REDISTRIBUTABLES = {
    b"vcruntime140": "vcrun2022", b"msvcp140": "vcrun2022",
    b"msvcp120": "vcrun2013", b"msvcr120": "vcrun2013",
    b"d3dx9_43": "d3dx9", b"d3dcompiler_47": "d3dcompiler_47",
    b"mscoree.dll": "dotnet48",
}
ENV_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
RESERVED_ENV = {"WINEPREFIX", "WINEARCH", "WINE", "WINESERVER", "XDG_DATA_HOME",
                "STEAM_COMPAT_DATA_PATH", "STEAM_COMPAT_CLIENT_INSTALL_PATH",
                "GAMEID", "PROTONPATH", "UMU_FOLDERS_PATH"}


def windows_path(path):
    return "Z:" + str(path).replace("/", "\\")


def windows_argv(command):
    """Split Windows command lines using CommandLineToArgvW backslash rules."""
    argv, word = [], []
    index = 0
    quoted = False
    started = False
    while index < len(command):
        character = command[index]
        if character == "\\":
            end = index
            while end < len(command) and command[end] == "\\":
                end += 1
            count = end - index
            if end < len(command) and command[end] == '"':
                word.append("\\" * (count // 2))
                if count % 2:
                    word.append('"')
                    end += 1
            else:
                word.append("\\" * count)
            index = end
            started = True
            continue
        if character == '"':
            if quoted and command[index:index + 2] == '""':
                word.append('"')
                index += 1
            else:
                quoted = not quoted
            started = True
        elif character.isspace() and not quoted:
            if started:
                argv.append("".join(word))
                word, started = [], False
        else:
            word.append(character)
            started = True
        index += 1
    if started:
        argv.append("".join(word))
    return argv


def environment(text):
    result = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        key, separator, value = line.partition("=")
        if not separator or not ENV_NAME.fullmatch(key) or key in RESERVED_ENV or "\0" in value:
            raise FlatapkError("Use one environment setting per line in NAME=value format.")
        result[key] = value
    return result


def ensure_runtime(runner, runtime, machine):
    if runtime == PROTON:
        if architecture_group(machine) != "x86_64":
            raise FlatapkError("Automatic Proton installation currently requires a 64-bit PC.")
        if runner.sandbox:
            bundled = Path("/app/bin/umu-run")
            if not bundled.is_file():
                raise FlatapkError("UMU Launcher is missing from this Packlift build.")
            target = runner.data / "umu-run"
            temporary = target.with_suffix(".tmp")
            shutil.copyfile(bundled, temporary)
            temporary.chmod(0o700)
            temporary.replace(target)
        elif not runner.available("umu-run"):
            raise FlatapkError("Install UMU Launcher on the host before using Proton.")
        return PROTON
    if runtime == "wine" and runner.available("wine"):
        return runtime
    if runtime not in {"wine", FLATPAK_WINE}:
        if not Path(runtime).is_absolute() or not os.access(runtime, os.X_OK):
            raise FlatapkError("Choose an executable Wine or Proton runtime installed on the host.")
        return runtime
    if architecture_group(machine) != "x86_64" or not runner.available("flatpak"):
        raise FlatapkError("Install Wine or choose an installed Wine or Proton in Advanced options.")
    installed = runner.run(["flatpak", "info", FLATPAK_WINE], streaming=False, check=False)
    if installed.returncode:
        runner.run(["flatpak", "remote-add", "--user", "--if-not-exists", "flathub",
                    FLATHUB_REPOSITORY])
        runner.run(["flatpak", "install", "--user", "--noninteractive", "flathub", FLATPAK_WINE])
    return FLATPAK_WINE


class Wine:
    def __init__(self, runner, runtime, machine):
        self.runner = runner
        self.root = runner.data / "wine"
        self.prefix = self.root / "prefix"
        self.integrations = self.root / "desktop"
        self.runtime = runtime or "wine"
        self.flatpak = self.runtime == FLATPAK_WINE
        self.umu = self.runtime == PROTON
        self.proton = self.umu or Path(self.runtime).name == "proton"
        self.architecture = "win32" if architecture_group(machine) == "x86" else "win64"

    def command(self, *arguments, extra=None, cwd=None, tool="wine"):
        values = {"WINEPREFIX": str(self.prefix)}
        if self.umu:
            values["GAMEID"] = "umu-default"
            values["UMU_FOLDERS_PATH"] = str(self.runner.data)
        if tool != "wineserver":
            values["XDG_DATA_HOME"] = str(self.integrations)
        values.update(extra or {})
        if self.umu:
            if self.runner.sandbox:
                return ["flatpak-spawn", "--host", f"--directory={cwd or Path.home()}",
                        *(f"--env={key}={value}" for key, value in values.items()),
                        str(self.runner.data / "umu-run"), *arguments]
            directory = ["-C", cwd] if cwd else []
            return ["env", *directory, *(f"{key}={value}" for key, value in values.items()),
                    "umu-run", *arguments]
        if self.flatpak:
            filesystem = [f"--filesystem={self.runner.data}"]
            directory = []
            if cwd:
                working = Path(cwd)
                if working.is_relative_to(Path.home().resolve()):
                    filesystem.append("--filesystem=home")
                else:
                    filesystem.append(f"--filesystem={working}")
                directory = [f"--cwd={working}"]
            return ["flatpak", "run", f"--command={tool}", *filesystem, *directory,
                    *(f"--env={key}={value}" for key, value in values.items()),
                    self.runtime, *arguments]
        directory = ["-C", cwd] if cwd else []
        if self.proton and not self.umu:
            values["STEAM_COMPAT_DATA_PATH"] = str(self.root / "compatdata")
            values["STEAM_COMPAT_CLIENT_INSTALL_PATH"] = str(Path.home() / ".steam/steam")
        wrapper = [self.runtime, "run"] if self.proton and not self.umu else [self.runtime]
        return ["env", *directory, *(f"{key}={value}" for key, value in values.items()),
                *wrapper, *arguments]

    def prepare(self):
        self.root.mkdir(parents=True, exist_ok=True)
        if self.runtime == "wine":
            if not self.runner.available("wine"):
                raise FlatapkError(
                    "Wine was not found on the host. Install Wine or choose an installed Wine or Proton in Advanced options.")
        elif not self.umu and not self.flatpak and (not Path(self.runtime).is_absolute() or not os.access(self.runtime, os.X_OK)):
            raise FlatapkError("Choose an executable Wine or Proton runtime installed on the host.")
        if self.proton and not self.umu:
            compatdata = self.root / "compatdata"
            compatdata.mkdir(exist_ok=True)
            self.prefix.mkdir(exist_ok=True)
            link = compatdata / "pfx"
            if not link.is_symlink():
                link.symlink_to(self.prefix, target_is_directory=True)
            elif link.resolve() != self.prefix:
                raise FlatapkError("Proton's prefix does not point to the Flatapk prefix.")
            self.run("wineboot.exe", "--init")
        elif self.umu or not (self.prefix / "system.reg").exists():
            extra = None if self.flatpak else {"WINEARCH": self.architecture}
            self.run("wineboot.exe", "--init", extra=extra)
        self.wait()
        for desktop in self.prefix.glob("drive_c/users/*/Desktop"):
            if desktop.is_symlink():
                desktop.unlink()
                desktop.mkdir()

    def run(self, *argv, extra=None):
        self.runner.run(self.command(*argv, extra=extra),
                        local=self.umu and self.runner.sandbox)

    def wait(self):
        if self.umu:
            self.runner.run(self.command("wineserver", "-w", tool="wineserver"),
                            local=self.runner.sandbox)
            return
        if self.flatpak:
            self.runner.run(self.command("-w", tool="wineserver"))
            return
        runtime = Path(self.runtime)
        server = "wineserver"
        if self.proton:
            candidates = [runtime.parent / name / "bin/wineserver" for name in ("files", "dist")]
            server = next((str(p) for p in candidates if os.access(p, os.X_OK)), server)
        elif runtime.is_absolute() and os.access(runtime.with_name("wineserver"), os.X_OK):
            server = str(runtime.with_name("wineserver"))
        self.runner.run(["env", f"WINEPREFIX={self.prefix}", server, "-w"])

    @contextmanager
    def backup(self, location):
        if self.prefix.exists():
            self.wait()
        location.mkdir(parents=True)
        for name in ("prefix", "desktop"):
            source = self.root / name
            if source.exists():
                subprocess.run(["cp", "-a", "--reflink=auto", "--", str(source),
                                str(location / name)], check=True)
        try:
            yield
        except Exception:
            if self.prefix.exists():
                try:
                    self.wait()
                except FlatapkError as error:
                    self.runner.write(str(error))
            self.restore(location)
            raise

    def restore(self, location):
        for name in ("prefix", "desktop"):
            source = location / name
            active = self.root / name
            discarded = location / f"{name}.discarded"
            if active.exists():
                active.rename(discarded)
            try:
                if source.exists():
                    source.rename(active)
            except OSError:
                if discarded.exists():
                    discarded.rename(active)
                raise
            if discarded.exists():
                shutil.rmtree(discarded)

    def registry(self):
        entries = {}
        for root in ("HKLM", "HKCU"):
            for key in (UNINSTALL_KEY, UNINSTALL_KEY.replace(r"\Windows", r"\Wow6432Node\Windows", 1)):
                output = self.runner.try_capture(self.command("reg.exe", "query", f"{root}\\{key}", "/s"))
                current = None
                for line in output.splitlines():
                    line = line.strip()
                    if line.startswith("HKEY_"):
                        current = entries.setdefault(line, {})
                    elif current is not None:
                        match = re.match(r"(\S+)\s+REG_\w+\s+(.*)", line)
                        if match:
                            current[match[1]] = match[2]
        return {key: values for key, values in entries.items() if values.get("UninstallString")}

    def shortcuts(self):
        result = {}
        for link in self.prefix.glob("drive_c/**/*.lnk"):
            relative = link.relative_to(self.prefix)
            location = relative.as_posix().lower()
            if "start menu" not in location and "/desktop/" not in location:
                continue
            if link.is_file() and link.stat().st_size < 1024 * 1024:
                result[link] = hashlib.sha256(link.read_bytes()).hexdigest()
        return result

    def program_files(self):
        files = set()
        for path in self.prefix.glob("drive_c/Program Files*/**/*"):
            if not path.is_symlink() and path.is_file():
                files.add(path)
        return files

    def dependencies(self, source):
        if self.proton:
            return
        found = set()
        with source.open("rb") as stream:
            tail = b""
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                sample = (tail + chunk).lower()
                found.update(verb for name, verb in REDISTRIBUTABLES.items() if name in sample)
                tail = chunk[-32:]
        if not found:
            return
        if self.flatpak:
            self.runner.run(self.command("-q", *sorted(found), tool="winetricks"))
            self.wait()
            return
        if not self.runner.available("winetricks"):
            raise FlatapkError("This installer needs Windows components. Install Winetricks on the host and retry.")
        self.runner.run(["env", f"WINEPREFIX={self.prefix}", f"WINE={self.runtime}",
                         "winetricks", "-q", *sorted(found)])
        self.wait()

    def launch_icon(self, name, directory, index):
        name = name.casefold()
        for desktop in self.integrations.glob("applications/**/*.desktop"):
            if desktop.is_symlink() or desktop.stat().st_size > 64 * 1024:
                continue
            details = desktop_details(desktop.read_text(encoding="utf-8", errors="replace"))
            if not details or details[0].casefold() != name:
                continue
            choices = [str(path) for path in self.integrations.glob("icons/**/*")
                       if path.is_file() and not path.is_symlink()]
            icon = icon_from_paths(choices, details[2])
            if icon:
                destination = directory / f"wine-icon-{index}{Path(icon).suffix}"
                shutil.copyfile(icon, destination)
                return str(destination)
        return ""

    def install(self, package, source, directory, step):
        self.prepare()
        previous_links = self.shortcuts()
        previous_registry = self.registry()
        previous_files = self.program_files()
        step("dependencies")
        self.dependencies(source)
        step("install")
        if package.kind == "msi":
            self.run("msiexec.exe", "/i", windows_path(source))
        else:
            self.run(str(source))
        self.wait()
        registry = self.registry()
        entries = {key: value for key, value in registry.items()
                   if previous_registry.get(key) != value}
        if package.product and not entries:
            entries = {key: value for key, value in registry.items() if
                       key.rsplit("\\", 1)[-1].lower() == package.product.lower()}
        installed_files = self.program_files() - previous_files
        owned_links = []
        launchers = []
        for link, digest in self.shortcuts().items():
            if previous_links.get(link) == digest:
                continue
            if link.is_relative_to(self.prefix) and link.resolve().is_relative_to(self.prefix):
                owned_links.append(str(link))
            if re.search(r"uninstall|desinstal|readme|website", link.stem, re.I):
                continue
            exported = directory / f"shortcut-{len(launchers)}.lnk"
            shutil.copyfile(link, exported)
            icon = self.launch_icon(link.stem, directory, len(launchers))
            launchers.append((link.stem, ["start.exe", "/unix", str(exported)], icon, False))
        if not launchers:
            executables = [path for path in sorted(installed_files) if path.suffix.lower() == ".exe"
                           and not re.search(r"unins|setup|update|helper", path.stem, re.I)]
            launchers = [(p.stem, [str(p)], "", False) for p in executables[:1]]
        if not launchers:
            raise FlatapkError("Wine did not produce a new program shortcut or executable.")
        uninstall = [value.get("QuietUninstallString") or value["UninstallString"]
                     for value in entries.values()]
        if not uninstall and package.product and re.fullmatch(r"\{[A-Fa-f0-9-]{36}\}", package.product):
            uninstall = [f"msiexec.exe /x {package.product}"]
        return launchers, {
            "runtime": self.runtime, "uninstall": uninstall, "links": owned_links,
            "installed_files": [str(p) for p in sorted(installed_files)],
        }

    def remove(self, app, other_apps):
        for command in app.get("uninstall", []):
            self.run(*windows_argv(command))
            self.wait()
        claimed = {path for app in other_apps for path in app.get("installed_files", [])}
        for filename in [*app.get("links", []), *app.get("installed_files", [])]:
            path = Path(filename)
            if filename not in claimed and path.resolve().is_relative_to(self.prefix):
                path.unlink(missing_ok=True)
                parent = path.parent
                while parent.name and parent != self.prefix / "drive_c" and parent.is_relative_to(self.prefix):
                    try:
                        parent.rmdir()
                    except OSError:
                        break
                    parent = parent.parent
