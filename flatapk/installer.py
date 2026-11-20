"""Coordinate package installation, recovery and registry changes."""

import hashlib
import shutil
import tempfile
import tarfile
import uuid
from pathlib import Path

from .core import FlatapkError, detect, fingerprint, host_info, inspect, plan_for
from .linux import (MIME_SUFFIXES, already_installed, install_local,
                    install_system, package_target, remove_system, system_launchers, toolbox)
from .metadata import decorate
from .system import Registry, Runner, data_home, export_desktop, operation, remove_exports, stage_package
from .wine import Wine, environment


class Installer:
    def __init__(self, output=lambda line: None, data=None, applications=None):
        self.data = Path(data) if data else data_home() / "flatapk"
        self.applications = Path(applications) if applications else data_home() / "applications"
        self.runner = Runner(self.data, output)
        self.registry = Registry(self.data)

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

    def prepare_exports(self, app, launchers, wine=None):
        app["exports"] = []
        for index, (name, argv, icon, terminal) in enumerate(launchers):
            desktop = self.applications / f"io.github.flatapk.Installer.{app['id']}.{index}.desktop"
            app["exports"].append({
                "desktop": str(desktop), "name": name, "icon": icon or "application-x-executable",
                "argv": wine.command(*argv, extra=app.get("environment")) if wine else argv,
                "terminal": terminal, "wine_args": argv if wine else [],
            })
        if not app["name"] or wine:
            app["name"] = app["exports"][0]["name"]

    def write_exports(self, app):
        for export in app["exports"]:
            export_desktop(Path(export["desktop"]), export["name"],
                           export["argv"], export["icon"], export["terminal"])

    def register(self, app, launchers, step, wine=None):
        step("shortcut")
        self.prepare_exports(app, launchers, wine)
        self.write_exports(app)
        self.registry.save(app)

    def uninstall(self, app, step):
        with operation(self.data):
            step("install")
            if app["method"] in {"native", "toolbox"}:
                if already_installed(self.runner, app["manager"], app["target"], app["container"]):
                    remove_system(self.runner, app["manager"], app["target"], app["container"])
            elif app["method"] == "wine":
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

    def configure(self, app, text):
        with operation(self.data):
            app["environment"] = environment(text)
            wine = Wine(self.runner, app["runtime"], host_info(self.runner)[1])
            for export in app["exports"]:
                export["argv"] = wine.command(*export["wine_args"], extra=app["environment"])
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
            apps = [app for app in self.registry.apps() if app["method"] == "wine"]
            if not apps:
                raise FlatapkError("There are no registered Windows programs to repair.")
            machine = host_info(self.runner)[1]
            primary = Wine(self.runner, apps[0]["runtime"], machine)
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
                    wine = Wine(self.runner, original["runtime"], machine)
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

