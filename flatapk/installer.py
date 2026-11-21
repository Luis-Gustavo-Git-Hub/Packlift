"""Coordinate package installation, recovery and registry changes."""

import hashlib
import json
import os
import shlex
import shutil
import subprocess
import tempfile
import tarfile
import uuid
from pathlib import Path

from .core import FlatapkError, Package, detect, fingerprint, host_info, inspect, plan_for
from .linux import (MIME_SUFFIXES, already_installed, install_local,
                    install_system, package_target, remove_system, system_launchers, toolbox,
                    write_appimage_icon)
from .metadata import appimage_assets, decorate
from .system import Registry, Runner, data_home, export_desktop, operation, remove_exports, stage_package
from .wine import FLATPAK_WINE, Wine, ensure_runtime, environment

DEFAULT_APP_ICON = "application-x-executable"


class Installer:
    def __init__(self, output=lambda line: None, data=None, applications=None):
        self.data = Path(data) if data else data_home() / "flatapk"
        self.applications = Path(applications) if applications else data_home() / "applications"
        self.preferences_path = self.data / "preferences.json"
        self.runner = Runner(self.data, output)
        self.registry = Registry(self.data)

    def preferences(self):
        try:
            value = json.loads(self.preferences_path.read_text(encoding="utf-8"))
            return {"runtime": value.get("runtime", "wine"),
                    "proton_path": value.get("proton_path", "")}
        except (OSError, ValueError, AttributeError):
            return {"runtime": "wine", "proton_path": ""}

    def save_preferences(self, runtime, proton_path):
        if runtime not in {"wine", "proton"}:
            raise FlatapkError("Choose Wine or Proton.")
        proton_path = str(Path(proton_path).expanduser()) if proton_path else ""
        if runtime == "proton" and proton_path and (not Path(proton_path).is_file()
                                                     or not os.access(proton_path, os.X_OK)):
            raise FlatapkError("The selected Proton executable is not available.")
        self.data.mkdir(mode=0o700, parents=True, exist_ok=True)
        temporary = self.preferences_path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"runtime": runtime, "proton_path": ""}),
                             encoding="utf-8")
        temporary.chmod(0o600)
        temporary.replace(self.preferences_path)

    def probe(self, path):
        package = inspect(path, self.runner)
        plan = plan_for(package, host_info(self.runner))
        try:
            decorate(package, self.runner)
        except (FlatapkError, OSError, tarfile.TarError) as error:
            self.runner.write(f"Optional display metadata could not be read: {error}")
        return package, plan

    def install(self, package, plan, step, select, runtime="wine"):
        step("analyze")
        with operation(self.data), tempfile.TemporaryDirectory(dir=self.data) as temp:
            staging = Path(temp)
            source = staging / f"package{MIME_SUFFIXES[package.kind]}"
            digest = stage_package(package, source)
            self.runner.write(f"SHA-256 {digest}  {package.path}")
            if package.fingerprint != fingerprint(package.path):
                raise FlatapkError("The source file changed after confirmation. Open it again.")
            actual_kind, _ = detect(source)
            expected = {"setup": "exe", "tar": "tar"}.get(package.kind, package.kind)
            if actual_kind != expected:
                raise FlatapkError("The source file changed after confirmation. Open it again.")
            if package.signature == "verified" and digest != package.verified_digest:
                raise FlatapkError("The signed source file changed after confirmation. Open it again.")
            step("compatibility")
            host = host_info(self.runner)
            if plan_for(package, host) != plan:
                raise FlatapkError("The system changed after confirmation. Open the package again.")
            identifier = hashlib.sha256(f"{package.kind}:{package.identifier or digest}:"
                                        f"{package.architecture}".encode()).hexdigest()[:20]
            app = {
                "id": identifier, "name": package.name, "version": package.version,
                "architecture": package.architecture, "method": plan.method,
                "manager": plan.manager, "source": str(package.path), "digest": digest,
                "directory": str(self.data / "installed" / f"{identifier}-{uuid.uuid4().hex[:8]}"), "exports": [],
            }
            if any(existing["id"] == identifier for existing in self.registry.apps()):
                raise FlatapkError("This program is already registered in Flatapk.")
            directory = Path(app["directory"])
            directory.mkdir(mode=0o700, parents=True)
            step("environment")
            container = target = ""
            attempted = False
            try:
                if plan.method in {"native", "toolbox"}:
                    container = toolbox(self.runner, plan.family, self.registry) if plan.method == "toolbox" else ""
                    target = package_target(package, plan.manager)
                    if already_installed(self.runner, plan.manager, target, container):
                        raise FlatapkError("The system already has this package installed.")
                    attempted = True
                    install_system(self.runner, package, plan, source, container, step)
                    app.update(container=container, target=target)
                    launchers = system_launchers(self.runner, package, plan, target, container, directory)
                    self.register(app, launchers, step)
                elif plan.method == "wine":
                    app["kind"] = package.kind
                    app["product"] = package.product
                    self.install_wine(app, package, source, directory, staging, runtime, host[1], step)
                else:
                    step("install")
                    launchers = install_local(package, source, directory, select, host[1])
                    self.register(app, launchers, step)
            except Exception:
                remove_exports(app)
                if attempted:
                    try:
                        if already_installed(self.runner, plan.manager, target, container):
                            remove_system(self.runner, plan.manager, target, container)
                    except FlatapkError as error:
                        self.runner.write(f"Package recovery failed: {error}")
                raise
        step("done")
        return app

    def install_wine(self, app, package, source, directory, staging, runtime, machine, step):
        runtime = self.windows_runtime(runtime, machine)
        wine = Wine(self.runner, runtime, machine)
        backup = staging / "snapshot"
        with wine.backup(backup):
            launchers, record = wine.install(package, source, directory, step)
            app.update(record)
            app["installer"] = str(directory / source.name)
            shutil.copyfile(source, app["installer"])
            self.register(app, launchers, step, wine)
        old = wine.root / "undo"
        candidate = wine.root / f"undo-{app['id']}"
        try:
            (backup / "app-id").write_text(app["id"], encoding="utf-8")
            backup.rename(candidate)
            if old.exists():
                shutil.rmtree(old)
            candidate.rename(old)
        except OSError as error:
            self.runner.write(f"Could not save the rollback snapshot: {error}")
            shutil.rmtree(candidate, ignore_errors=True)

    def windows_runtime(self, runtime, machine):
        if runtime == "wine" and any(app.get("runtime") == FLATPAK_WINE for app in self.registry.apps()):
            runtime = FLATPAK_WINE
        return ensure_runtime(self.runner, runtime, machine)

    def add_game(self, package, step, runtime="wine"):
        source = package.path
        step("analyze")
        with operation(self.data):
            if package.kind != "exe" or not source.is_file():
                raise FlatapkError("Choose the Windows executable from the game's folder.")
            if fingerprint(source) != package.fingerprint:
                raise FlatapkError("The source file changed after confirmation. Open it again.")
            if package.checksum or package.verified_digest:
                with source.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
                if any(expected and expected != digest for expected in (package.verified_digest, package.checksum)):
                    raise FlatapkError("The file does not match its SHA-256 checksum.")
            step("compatibility")
            host = host_info(self.runner)
            plan_for(package, host)
            identifier = hashlib.sha256(f"game:{source}".encode()).hexdigest()[:20]
            if any(app["id"] == identifier for app in self.registry.apps()):
                raise FlatapkError("This program is already registered in Flatapk.")
            step("environment")
            wine = Wine(self.runner, self.windows_runtime(runtime, host[1]), host[1])
            if (not (wine.prefix / "system.reg").is_file()
                    or (wine.proton and not wine.umu
                        and not (wine.root / "compatdata" / "pfx").is_symlink())
                    or wine.umu):
                wine.prepare()
            if fingerprint(source) != package.fingerprint:
                raise FlatapkError("The source file changed after confirmation. Open it again.")
            app = {
                "id": identifier, "name": package.name, "version": package.version,
                "architecture": package.architecture, "source": str(source), "exports": [],
                "method": "wine", "runtime": wine.runtime, "game_shortcut": True,
                "working_directory": str(source.parent),
            }
            icon = ""
            if package.icon:
                directory = self.data / "games" / identifier
                directory.mkdir(mode=0o700, parents=True, exist_ok=True)
                icon_path = directory / "icon.ico"
                icon_path.write_bytes(package.icon)
                app.update(directory=str(directory), icon_path=str(icon_path))
                icon = str(icon_path)
            try:
                self.register(app, [(package.name, [str(source)], icon, False)], step, wine)
            except Exception:
                remove_exports(app)
                raise
        step("done")
        return app

    def prepare_exports(self, app, launchers, wine=None):
        app["exports"] = []
        for index, (name, argv, icon, terminal) in enumerate(launchers):
            desktop = self.applications / f"io.github.flatapk.Installer.{app['id']}.{index}.desktop"
            app["exports"].append({
                "desktop": str(desktop), "name": name, "icon": icon or DEFAULT_APP_ICON,
                "argv": wine.command(*argv, extra=app.get("environment"),
                                     cwd=app.get("working_directory")) if wine else argv,
                "terminal": terminal, "wine_args": argv if wine else [],
            })
        if not app["name"] or wine:
            app["name"] = app["exports"][0]["name"]

    def write_exports(self, app):
        category = "Game" if app.get("game_shortcut") else "Utility"
        for export in app["exports"]:
            export_desktop(Path(export["desktop"]), export["name"],
                           export["argv"], export["icon"], export["terminal"], category=category)

    def register(self, app, launchers, step, wine=None):
        step("shortcut")
        self.prepare_exports(app, launchers, wine)
        self.write_exports(app)
        self.registry.save(app)

    def appimages_missing_icons(self):
        return [app for app in self.registry.apps()
                if app["method"] == "local"
                and (Path(app["directory"]) / "program.AppImage").is_file()
                and any(export["icon"] == DEFAULT_APP_ICON for export in app["exports"])]

    def games_missing_icons(self):
        return [app for app in self.registry.apps()
                if app.get("game_shortcut")
                and any(export["icon"] == DEFAULT_APP_ICON for export in app["exports"])
                and Path(app.get("source", "")).is_file()]

    def refresh_game_icons(self):
        try:
            from .metadata import pe_icon

            with operation(self.data):
                for app in self.games_missing_icons():
                    icon = pe_icon(Path(app["source"]))
                    if not icon:
                        continue
                    directory = self.data / "games" / app["id"]
                    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
                    icon_path = directory / "icon.ico"
                    icon_path.write_bytes(icon)
                    app.update(directory=str(directory), icon_path=str(icon_path))
                    for export in app["exports"]:
                        if export["icon"] == DEFAULT_APP_ICON:
                            export["icon"] = str(icon_path)
                    self.write_exports(app)
                    self.registry.save(app)
        except FlatapkError as error:
            self.runner.write(str(error))

    def refresh_appimage_icons(self):
        try:
            with operation(self.data):
                for app in self.appimages_missing_icons():
                    directory = Path(app["directory"])
                    package = Package(directory / "program.AppImage", "appimage", app["name"])
                    try:
                        appimage_assets(package, self.runner)
                        icon = write_appimage_icon(package, directory)
                        if not icon:
                            continue
                        for export in app["exports"]:
                            if export["icon"] == DEFAULT_APP_ICON:
                                export["icon"] = icon
                        self.write_exports(app)
                        self.registry.save(app)
                    except OSError as error:
                        self.runner.write(f"Could not refresh the icon for {app['name']}: {error}")
        except FlatapkError as error:
            self.runner.write(str(error))

    def observe_launch(self, app):
        export = app["exports"][0]
        argv = self.runner.command(export["argv"], local=False, watch_bus=False)
        self.runner.write(f"$ {shlex.join(argv)}")
        # A file descriptor lets an app keep running if the Flatapk window closes.
        with tempfile.TemporaryFile() as log:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            status = process.wait()
            if status == 0:
                return
            size = log.seek(0, os.SEEK_END)
            log.seek(max(0, size - 16 * 1024))
            output = log.read().decode("utf-8", errors="replace").strip()
            if output:
                self.runner.write(output)
            self.runner.write(f"Program exited with status {status}.")
            raise FlatapkError("The program exited with an error. See the launch output in the log.")

    def uninstall(self, app, step):
        with operation(self.data):
            step("install")
            if app["method"] in {"native", "toolbox"}:
                if already_installed(self.runner, app["manager"], app["target"], app["container"]):
                    remove_system(self.runner, app["manager"], app["target"], app["container"])
            elif app["method"] == "wine" and not app.get("game_shortcut"):
                machine = host_info(self.runner)[1]
                wine = Wine(self.runner, app["runtime"], machine)
                others = [entry for entry in self.registry.apps() if entry["id"] != app["id"]]
                with tempfile.TemporaryDirectory(dir=self.data) as temp, wine.backup(Path(temp) / "snapshot"):
                    wine.remove(app, others)
                shutil.rmtree(wine.root / "undo", ignore_errors=True)
            remove_exports(app)
            self.registry.remove(app)
        step("done")

    def clean(self, step):
        with operation(self.data):
            in_use = {app.get("container") for app in self.registry.apps()}
            for name in self.registry.containers():
                if name in in_use:
                    continue
                step("environment")
                result = self.runner.run(["podman", "container", "exists", name], check=False)
                if not result.returncode:
                    self.runner.run(["toolbox", "rm", "--force", name])
                elif result.returncode != 1:
                    raise FlatapkError("Podman could not check the Toolbox environment.")
                self.registry.remove_container(name)
        step("done")

    def configure(self, app, text, runtime=None):
        with operation(self.data):
            app["environment"] = environment(text)
            machine = host_info(self.runner)[1]
            runtime = self.windows_runtime(runtime or app["runtime"], machine)
            wine = Wine(self.runner, runtime, machine)
            if runtime != app["runtime"]:
                wine.prepare()
            app["runtime"] = runtime
            for export in app["exports"]:
                export["argv"] = wine.command(*export["wine_args"], extra=app["environment"],
                                             cwd=app.get("working_directory"))
            self.write_exports(app)
            self.registry.save(app)

    def undo_wine(self, step):
        with operation(self.data):
            undo = self.data / "wine" / "undo"
            marker = undo / "app-id"
            if not marker.exists():
                raise FlatapkError("There is no Wine installation snapshot to restore.")
            identifier = marker.read_text(encoding="utf-8")
            app = next((item for item in self.registry.apps() if item["id"] == identifier), None)
            if app is None:
                raise FlatapkError("The snapshot's program is no longer registered.")
            step("environment")
            wine = Wine(self.runner, app["runtime"], host_info(self.runner)[1])
            wine.wait()
            wine.restore(undo)
            remove_exports(app)
            self.registry.remove(app)
            shutil.rmtree(undo)
        step("done")

    def repair_wine(self, step):
        with operation(self.data), tempfile.TemporaryDirectory(dir=self.data) as temp:
            apps = [app for app in self.registry.apps()
                    if app["method"] == "wine" and not app.get("game_shortcut")]
            if not apps:
                raise FlatapkError("There are no registered Windows programs to repair.")
            machine = host_info(self.runner)[1]
            primary = Wine(self.runner, self.windows_runtime(apps[0]["runtime"], machine), machine)
            staging = Path(temp)
            prepared = []
            step("environment")
            with primary.backup(staging / "snapshot"):
                for folder in (primary.prefix, primary.integrations):
                    if folder.exists():
                        folder.rename(staging / folder.name)
                for original in apps:
                    source = Path(original["installer"])
                    with source.open("rb") as stream:
                        digest = hashlib.file_digest(stream, "sha256").hexdigest()
                    if digest != original["digest"]:
                        raise FlatapkError("A stored Windows installer failed its integrity check.")
                    package = inspect(source, self.runner)
                    wine = Wine(self.runner, self.windows_runtime(original["runtime"], machine), machine)
                    directory = Path(original["directory"]).with_name(
                        original["id"] + "-" + uuid.uuid4().hex[:8])
                    record = dict(original, directory=str(directory), installer=str(directory / source.name))
                    directory.mkdir()
                    prepared.append(record)
                    try:
                        launchers, details = wine.install(package, source, directory, step)
                        shutil.copyfile(source, record["installer"])
                        record.update(details)
                        self.prepare_exports(record, launchers, wine)
                    except Exception:
                        for app in prepared:
                            shutil.rmtree(app["directory"], ignore_errors=True)
                        raise
                step("shortcut")
                try:
                    for app in prepared:
                        self.write_exports(app)
                    self.registry.save_many(prepared)
                except Exception:
                    for app in prepared:
                        remove_exports(app)
                    for app in apps:
                        self.write_exports(app)
                    raise
            still_exported = {entry["desktop"] for app in prepared for entry in app["exports"]}
            for app in apps:
                app["exports"] = [entry for entry in app["exports"] if entry["desktop"] not in still_exported]
                remove_exports(app)
            shutil.rmtree(primary.root / "undo", ignore_errors=True)
        step("done")
