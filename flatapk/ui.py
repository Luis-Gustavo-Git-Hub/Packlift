"""GTK4 / libadwaita application."""

import gettext
import os
import subprocess
import sys
import threading
from collections import deque
from pathlib import Path

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

from .core import FlatapkError
from .installer import Installer

APP_ID = "io.github.flatapk.Installer"
LOCALE = Path(__file__).resolve().parent.parent / "locale"
_ = gettext.translation("flatapk", localedir=LOCALE, fallback=True).gettext


def stages():
    return {
        "analyze": (0.12, _("Analyzing the file")),
        "compatibility": (0.25, _("Checking compatibility")),
        "environment": (0.38, _("Preparing the environment")),
        "dependencies": (0.52, _("Downloading dependencies")),
        "install": (0.70, _("Applying changes")),
        "shortcut": (0.90, _("Updating application shortcuts")),
        "done": (1.0, _("Done")),
    }


def methods():
    return {
        "native": _("System package manager"),
        "toolbox": _("Toolbox container"),
        "wine": _("Wine shared prefix"),
        "local": _("Local installation"),
    }


def label(text, style="", xalign=0.0):
    widget = Gtk.Label(label=text, xalign=xalign, wrap=True)
    if style:
        widget.add_css_class(style)
    return widget


def button(text, action, style=""):
    widget = Gtk.Button.new_with_mnemonic(text)
    if style:
        widget.add_css_class(style)
    widget.connect("clicked", lambda _: action())
    return widget


def vertical(spacing=12):
    return Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=spacing)


def action_bar(*buttons):
    bar = Gtk.Box(spacing=8, halign=Gtk.Align.END)
    for widget in buttons:
        bar.append(widget)
    return bar


class Window(Adw.ApplicationWindow):
    def __init__(self, app, width=440, height=280):
        super().__init__(application=app, title="Flatapk", default_width=width,
                         default_height=height)
        self.running = False
        self.log = deque(maxlen=2500)
        self.installer = Installer(self.on_log)
        self.connect("close-request", lambda _: self.running)
        self.view = Adw.ToolbarView()
        self.header = Adw.HeaderBar()
        self.header.set_title_widget(Adw.WindowTitle(title="Flatapk"))
        self.view.add_top_bar(self.header)
        self.set_content(self.view)
        self.reset()

    def reset(self):
        self.set_default_widget(None)
        self.log_buffer = None
        self.body = vertical()
        self.body.set_margin_start(20)
        self.body.set_margin_end(20)
        self.body.set_margin_top(14)
        self.body.set_margin_bottom(18)
        scroll = Gtk.ScrolledWindow(hscrollbar_policy=Gtk.PolicyType.NEVER)
        scroll.set_child(self.body)
        self.view.set_content(scroll)

    def on_log(self, line):
        GLib.idle_add(self.append_log, line)

    def append_log(self, line):
        self.log.append(line)
        if self.log_buffer is not None:
            self.log_buffer.set_text("\n".join(self.log))

    def run_job(self, task, finished):
        self.running = True

        def work():
            try:
                result = task()
            except Exception as error:
                GLib.idle_add(complete, None, error)
            else:
                GLib.idle_add(complete, result, None)

        def complete(result, error):
            self.running = False
            if error:
                self.show_result(error=error)
            else:
                finished(result)

        threading.Thread(target=work, daemon=True).start()

    def details(self):
        buffer = Gtk.TextBuffer()
        buffer.set_text("\n".join(self.log))
        self.log_buffer = buffer
        text = Gtk.TextView(buffer=buffer, editable=False, monospace=True,
                            cursor_visible=False, wrap_mode=Gtk.WrapMode.WORD_CHAR)
        scroll = Gtk.ScrolledWindow(min_content_height=150)
        scroll.set_child(text)
        expander = Gtk.Expander(label=_("Show details"))
        expander.set_child(scroll)
        self.body.append(expander)
        self.body.append(button(_("_Copy log"), lambda: self.get_clipboard().set("\n".join(self.log))))

    def shell(self, heading, description, icon_name):
        row = Gtk.Box(spacing=12)
        icon = Gtk.Image.new_from_icon_name(icon_name)
        icon.set_pixel_size(42)
        row.append(icon)
        words = vertical(4)
        words.append(label(heading, "title-2"))
        if description:
            words.append(label(description, "dim-label"))
        row.append(words)
        self.body.append(row)
        return icon

    def open_package(self, path):
        self.reset()
        self.shell(_("Open package"), path.name, "package-x-generic-symbolic")
        spinner = Gtk.Spinner(spinning=True, halign=Gtk.Align.CENTER)
        self.body.append(spinner)
        self.run_job(lambda: self.installer.probe(path), self.confirm)
        self.present()

    def confirm(self, result):
        package, plan = result
        self.reset()
        icon = self.shell(package.name, package.version or package.path.name,
                          "package-x-generic-symbolic")
        if package.icon:
            try:
                icon.set_from_paintable(Gdk.Texture.new_from_bytes(GLib.Bytes.new(package.icon)))
            except GLib.Error:
                pass
        if package.description:
            self.body.append(label(package.description))
        size = GLib.format_size(package.fingerprint[2])
        architecture = package.architecture or _("Unknown architecture")
        self.body.append(label(f"{package.path.name}  ·  {size}  ·  {package.kind.upper()}  ·  {architecture}", "dim-label"))
        method = methods()[plan.method]
        if plan.family and plan.method == "toolbox":
            method += f" · {plan.family.capitalize()}"
        self.body.append(label(method))
        origin = label(str(package.path.parent), "dim-label")
        origin.set_selectable(True)
        self.body.append(origin)
        if package.signature == "verified":
            self.body.append(label(_("Signature verified with a trusted key."), "success"))
        else:
            self.body.append(label(_("Signature could not be verified. Install only if you trust the source."), "warning"))
        if plan.method == "native":
            self.body.append(label(_("The system will ask you to authorize this installation with polkit.")))
        elif plan.method in {"toolbox", "wine"}:
            self.body.append(label(_("The program can access your home folder.")))
        if plan.multilib:
            self.body.append(label(_("Extra 32-bit components may be downloaded.")))
        runtime = Gtk.Entry(placeholder_text=_("Default Wine, or an absolute path to wine or proton"))
        options = Gtk.Expander(label=_("Advanced options"))
        advanced = vertical(8)
        if package.requires:
            advanced.append(label(_("Dependencies") + "\n" + ", ".join(package.requires)))
        if plan.method == "wine":
            advanced.append(runtime)
        if package.requires or plan.method == "wine":
            options.set_child(advanced)
            self.body.append(options)
        def install_package():
            selected_runtime = runtime.get_text().strip() or "wine"
            self.begin(_("Installing {}").format(package.name), lambda: self.installer.install(
                package, plan, self.step, self.select_executable, selected_runtime))

        install = button(_("_Install"), install_package, "suggested-action")
        self.body.append(action_bar(button(_("_Cancel"), self.close), install))
        self.set_default_widget(install)
        install.grab_focus()

    def select_executable(self, options, directory):
        event = threading.Event()
        selection = []

        def choose():
            dialog = Adw.AlertDialog(heading=_("Choose the program to launch"),
                                     body=_("This archive contains more than one executable."))
            menu = Gtk.DropDown.new_from_strings([str(path.relative_to(directory)) for path in options])
            dialog.set_extra_child(menu)
            dialog.add_response("cancel", _("_Cancel"))
            dialog.add_response("choose", _("_Choose"))
            dialog.set_response_appearance("choose", Adw.ResponseAppearance.SUGGESTED)
            dialog.set_default_response("choose")
            dialog.set_close_response("cancel")

            def respond(_, response):
                if response == "choose":
                    selection.append(options[menu.get_selected()])
                event.set()

            dialog.connect("response", respond)
            dialog.present(self)

        GLib.idle_add(choose)
        event.wait()
        return next(iter(selection), None)

    def begin(self, title, task):
        self.reset()
        self.shell(title, "", "software-install-symbolic")
        self.status = label(_("Preparing the environment"))
        self.progress = Gtk.ProgressBar(show_text=True)
        self.body.append(self.status)
        self.body.append(self.progress)
        self.details()
        self.run_job(task, lambda result: self.show_result(app=result))

    def step(self, stage):
        def update():
            fraction, text = stages()[stage]
            self.progress.set_fraction(fraction)
            self.status.set_text(text)

        GLib.idle_add(update)

    def show_result(self, app=None, error=None):
        self.reset()
        if error:
            message = _(str(error)) if isinstance(error, FlatapkError) else _(
                "Flatapk could not complete this operation.")
            self.append_log(str(error))
            self.shell(_("Operation failed"), message, "dialog-error-symbolic")
            self.body.append(label(_("Check the details for the failing step and try again."), "dim-label"))
        else:
            self.shell(_("Done"), app["name"] if app else "", "emblem-ok-symbolic")
        self.details()
        actions = [button(_("_Close"), self.close)]
        if app and app["exports"]:
            actions.append(button(_("_Open program"), lambda: self.launch(app), "suggested-action"))
        self.body.append(action_bar(*actions))

    def launch(self, app):
        export = app["exports"][0]
        if (app["method"] == "local" and not export["terminal"]
                and (Path(app["directory"]) / "program.AppImage").is_file()):
            threading.Thread(target=self.watch_appimage, args=(app,), daemon=True).start()
            return
        try:
            if not self.installer.runner.sandbox:
                info = Gio.DesktopAppInfo.new_from_filename(export["desktop"])
                if info is None or not info.launch([], None):
                    raise FlatapkError("The application launcher could not be started.")
            else:
                argv = self.installer.runner.command(["gio", "launch", export["desktop"]], local=False)
                subprocess.Popen(argv, start_new_session=True)
        except (GLib.Error, OSError, FlatapkError) as error:
            self.show_result(error=error)

    def watch_appimage(self, app):
        try:
            self.installer.launch_appimage(app)
        except (OSError, FlatapkError) as error:
            def show_error(launch_error):
                if self.get_visible() and not self.running:
                    self.show_result(error=launch_error)

            GLib.idle_add(show_error, error)

    def confirm_action(self, heading, description, action, destructive=False, response_text=None):
        dialog = Adw.AlertDialog(heading=heading, body=description)
        dialog.add_response("cancel", _("_Cancel"))
        dialog.add_response("continue", response_text or _("_Continue"))
        dialog.set_close_response("cancel")
        if destructive:
            dialog.set_response_appearance("continue", Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.connect("response::continue", lambda *_: action())
        dialog.present(self)

    def settings(self, app):
        dialog = Adw.AlertDialog(heading=_("Wine launch settings"),
                                 body=_("Enter one NAME=value per line, including WINEDLLOVERRIDES if needed."))
        entry = Gtk.TextView(monospace=True, height_request=96)
        buffer = entry.get_buffer()
        buffer.set_text("\n".join(f"{key}={value}" for key, value in app.get("environment", {}).items()))
        dialog.set_extra_child(entry)
        dialog.add_response("cancel", _("_Cancel"))
        dialog.add_response("save", _("_Save"))
        dialog.set_close_response("cancel")
        def save(*_):
            text = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
            self.run_job(lambda: self.installer.configure(app, text), lambda _: self.manage())

        dialog.connect("response::save", save)
        dialog.present(self)

    def file_dialog(self):
        dialog = Gtk.FileDialog(title=_("Open package"))

        def opened(dialog, result):
            try:
                file = dialog.open_finish(result)
            except GLib.Error:
                return
            if file and file.get_path():
                Window(self.get_application()).open_package(Path(file.get_path()))

        dialog.open(self, None, opened)

    def manage(self):
        self.reset()
        self.set_default_size(540, 390)
        group = Adw.PreferencesGroup(title=_("Installed programs"))
        self.body.append(group)
        apps = self.installer.registry.apps()
        if not apps:
            self.body.append(label(_("Open a package file to install your first program."), "dim-label"))
        for app in sorted(apps, key=lambda item: item["name"].casefold()):
            row = Adw.ActionRow(title=GLib.markup_escape_text(app["name"]),
                                subtitle=GLib.markup_escape_text(methods()[app["method"]]))
            row.add_prefix(Gtk.Image.new_from_icon_name("application-x-executable-symbolic"))
            row.add_suffix(button(_("_Open"), lambda app=app: self.launch(app)))
            row.add_suffix(button(_("_Uninstall"), lambda app=app: self.confirm_action(
                _("Uninstall {}?").format(app["name"]), _("This removes the program and its shortcuts."),
                lambda: self.begin(_("Uninstalling"), lambda: self.installer.uninstall(app, self.step)),
                destructive=True, response_text=_("_Uninstall"))))
            if app["method"] == "wine":
                row.add_suffix(button(_("_Settings"), lambda app=app: self.settings(app)))
            group.add(row)
        self.body.append(button(_("_Open package"), self.file_dialog, "suggested-action"))
        self.body.append(button(_("Clean _unused Toolbox environments"), lambda: self.confirm_action(
            _("Clean unused Toolbox environments?"),
            _("Flatapk removes its Toolbox containers that have no registered programs."),
            lambda: self.begin(_("Cleaning environments"), lambda: self.installer.clean(self.step)))))
        if any(app["method"] == "wine" for app in apps):
            self.body.append(button(_("_Repair Wine prefix"), lambda: self.confirm_action(
                _("Rebuild the shared Wine prefix?"),
                _("This runs every saved installer again. Settings and files inside the old prefix can be lost."),
                lambda: self.begin(_("Repairing Wine prefix"), lambda: self.installer.repair_wine(self.step)),
                destructive=True)))
        if (self.installer.data / "wine/undo/app-id").is_file():
            self.body.append(button(_("Undo last _Wine installation"), lambda: self.confirm_action(
                _("Restore the last Wine snapshot?"),
                _("This removes the most recently installed Windows program and restores the saved prefix."),
                lambda: self.begin(_("Restoring Wine prefix"), lambda: self.installer.undo_wine(self.step)),
                destructive=True)))


class Application(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID,
                         flags=Gio.ApplicationFlags.HANDLES_OPEN | Gio.ApplicationFlags.NON_UNIQUE)

    def do_activate(self):
        window = Window(self)
        window.manage()
        window.present()

    def do_open(self, files, count, hint):
        for file in files:
            window = Window(self)
            if file.get_path():
                window.open_package(Path(file.get_path()))
            else:
                window.show_result(error=FlatapkError("Choose a local package file."))
                window.present()


def main():
    return Application().run(sys.argv)
