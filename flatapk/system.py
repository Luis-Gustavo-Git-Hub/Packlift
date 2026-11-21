"""Host commands, durable installation registry, and exported desktop entries."""

import contextlib
import datetime
import fcntl
import hashlib
import json
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
from pathlib import Path

from .core import FlatapkError, Package


def data_home() -> Path:
    home = Path.home() / ".local/share"
    if Path("/.flatpak-info").exists():
        return Path(os.environ.get("HOST_XDG_DATA_HOME", str(home)))
    return Path(os.environ.get("XDG_DATA_HOME", str(home)))


class Runner:
    def __init__(self, data: Path, output=lambda line: None):
        self.data = data
        self.output = output
        self.sandbox = Path("/.flatpak-info").exists()
        data.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.log = data / "flatapk.log"

    def command(self, argv, local, *, watch_bus=True):
        if self.sandbox and argv and str(argv[0]) == "flatpak-spawn":
            return [str(item) for item in argv]
        if self.sandbox and not local:
            watch = ["--watch-bus"] if watch_bus else []
            return ["flatpak-spawn", "--host", *watch, f"--directory={Path.home()}", *argv]
        return argv

    def write(self, text):
        with self.log.open("a", encoding="utf-8") as stream:
            stream.write(text + "\n")
        self.output(text)

    def run(self, argv, *, local=False, streaming=True, check=True) -> subprocess.CompletedProcess:
        command = self.command([str(item) for item in argv], local)
        self.write(f"{datetime.datetime.now().isoformat(timespec='seconds')} $ {shlex.join(command)}")
        try:
            with subprocess.Popen(
                command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT if streaming else subprocess.PIPE,
                text=True, errors="replace",
            ) as process:
                if streaming:
                    for line in process.stdout:
                        self.write(line.rstrip("\n"))
                    process.wait()
                    output = ""
                else:
                    output, error = process.communicate()
                    if error:
                        self.write(error.rstrip())
                result = subprocess.CompletedProcess(command, process.returncode, output)
        except FileNotFoundError as error:
            raise FlatapkError(f"Required command is missing: {error.filename}.") from error
        if check and result.returncode:
            raise FlatapkError(
                f"{argv[0]} exited with status {result.returncode}. See the installation log.")
        return result

    def copy_stdout(self, argv, target, *, local=False):
        command = self.command([str(item) for item in argv], local)
        self.write(f"$ {shlex.join(command)}")
        with target.open("wb") as stream:
            try:
                subprocess.run(command, stdout=stream, stderr=subprocess.PIPE, check=True)
            except (OSError, subprocess.CalledProcessError) as error:
                target.unlink(missing_ok=True)
                raise FlatapkError(f"Could not export data using {argv[0]}.") from error

    def capture(self, argv, *, local=False):
        return self.run(argv, local=local, streaming=False).stdout

    def try_capture(self, argv, *, local=False):
        try:
            return self.capture(argv, local=local)
        except FlatapkError:
            return ""

    def available(self, command):
        result = self.run(["/bin/sh", "-c", 'command -v -- "$1"', "flatapk", command],
                          streaming=False, check=False)
        return result.returncode == 0


@contextlib.contextmanager
def operation(data: Path):
    data.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (data / "operation.lock").open("a") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise FlatapkError("Another Flatapk operation is running. Try again when it finishes.") from error
        yield


class Registry:
    def __init__(self, data):
        self.path = data / "apps.sqlite"
        data.mkdir(mode=0o700, parents=True, exist_ok=True)
        with self.connect() as db:
            db.execute("CREATE TABLE IF NOT EXISTS apps (id TEXT PRIMARY KEY, record TEXT NOT NULL)")
            db.execute("CREATE TABLE IF NOT EXISTS containers (name TEXT PRIMARY KEY)")

    @contextlib.contextmanager
    def connect(self):
        with contextlib.closing(sqlite3.connect(self.path)) as db, db:
            yield db

    def apps(self):
        with self.connect() as db:
            return [json.loads(row[0]) for row in db.execute("SELECT record FROM apps ORDER BY id")]

    def save(self, app):
        with self.connect() as db:
            db.execute("INSERT INTO apps VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                       (app["id"], json.dumps(app, ensure_ascii=False)))

    def save_many(self, apps):
        with self.connect() as db:
            db.executemany("INSERT INTO apps VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                           [(app["id"], json.dumps(app, ensure_ascii=False)) for app in apps])

    def remove(self, app):
        with self.connect() as db:
            db.execute("DELETE FROM apps WHERE id = ?", (app["id"],))

    def containers(self):
        with self.connect() as db:
            return [row[0] for row in db.execute("SELECT name FROM containers")]

    def add_container(self, name):
        with self.connect() as db:
            db.execute("INSERT OR IGNORE INTO containers VALUES (?)", (name,))

    def remove_container(self, name):
        with self.connect() as db:
            db.execute("DELETE FROM containers WHERE name = ?", (name,))


def stage_package(package: Package, target: Path) -> str:
    digest = hashlib.sha256()
    with package.path.open("rb") as source, target.open("xb") as destination:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            destination.write(chunk)
            digest.update(chunk)
    value = digest.hexdigest()
    if package.checksum and package.checksum != value:
        raise FlatapkError("The file does not match its SHA-256 checksum.")
    return value


def desktop_string(value):
    return str(value).replace("\\", "\\\\").replace("\n", " ").replace("\r", " ").replace("\t", " ")


def desktop_exec(argv):
    parts = []
    for item in map(str, argv):
        item = item.replace("%", "%%")
        if not re.fullmatch(r"[a-zA-Z0-9_./:@=+,%~-]+", item):
            item = '"' + re.sub(r'(["\\$`])', r'\\\1', item) + '"'
        parts.append(desktop_string(item))
    return " ".join(parts)


def export_desktop(path: Path, name, argv, icon, terminal=False, *, category="Utility"):
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = ["[Desktop Entry]", "Type=Application", f"Name={desktop_string(name)}",
             f"Exec={desktop_exec(argv)}", f"Icon={desktop_string(icon)}",
             f"Terminal={'true' if terminal else 'false'}", f"Categories={category};",
             "X-Flatapk-Managed=true", ""]
    temporary = path.with_suffix(".tmp")
    temporary.write_text("\n".join(lines), encoding="utf-8")
    temporary.replace(path)


def remove_exports(app):
    for export in app.get("exports", []):
        Path(export["desktop"]).unlink(missing_ok=True)
    if app.get("directory"):
        shutil.rmtree(app["directory"], ignore_errors=True)
