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
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Adw, Gdk, GdkPixbuf, Gio, GLib, Gtk, Pango

from .core import FlatapkError
from .installer import DEFAULT_APP_ICON, Installer

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
    widget.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
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


def menu_button(tooltip, actions):
    widget = Gtk.MenuButton(icon_name="view-more-symbolic", tooltip_text=tooltip,
                            valign=Gtk.Align.CENTER, css_classes=["flat"])
    popover = Gtk.Popover()
    entries = vertical(2)
    for title, action in actions:
        item = Gtk.Button(child=label(title), css_classes=["flat"])

        def activate(_, callback=action):
            popover.popdown()
            callback()

        item.connect("clicked", activate)
        entries.append(item)
    popover.set_child(entries)
    widget.set_popover(popover)
    return widget


def installed_icon(name):
    if Path(name).is_absolute():
        try:
            pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_scale(name, 32, 32, True)
            return Gtk.Image.new_from_pixbuf(pixbuf)
        except GLib.Error:
            name = DEFAULT_APP_ICON
    icon = Gtk.Image.new_from_icon_name(name)
    icon.set_pixel_size(32)
    return icon


class Window(Adw.ApplicationWindow):
    def __init__(self, app, *, compact=True):
        super().__init__(application=app, title="Packlift", default_width=466 if compact else 640,
                         default_height=370 if compact else 470, resizable=not compact)
        self.compact = compact
        self.running = False
        self.log = deque(maxlen=2500)
        self.installer = Installer(self.on_log)
        self.connect("close-request", lambda _: self.running)
        self.view = Adw.ToolbarView()
        self.header = Adw.HeaderBar()
        self.header.set_title_widget(Adw.WindowTitle(title="Packlift"))
        self.view.add_top_bar(self.header)
        self.footer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, css_classes=["flatapk-footer"])
        self.view.add_bottom_bar(self.footer)
        self.set_content(self.view)
        self.reset()

    def reset(self):
        self.set_default_widget(None)
        self.set_actions()
        for controls in getattr(self, "header_controls", []):
            self.header.remove(controls)
        self.header_controls = []
        self.log_buffer = None
        self.body = vertical(9 if self.compact else 12)
        self.body.set_margin_start(18)
        self.body.set_margin_end(18)
        self.body.set_margin_top(12 if self.compact else 16)
        self.body.set_margin_bottom(12 if self.compact else 16)
        scroll = Gtk.ScrolledWindow(hscrollbar_policy=Gtk.PolicyType.NEVER)
        clamp = Adw.Clamp(maximum_size=550, tightening_threshold=430)
        clamp.set_child(self.body)
        scroll.set_child(clamp)
        self.view.set_content(scroll)

    def set_actions(self, *buttons):
        if child := self.footer.get_first_child():
            self.footer.remove(child)
        self.footer.set_visible(bool(buttons))
        if buttons:
            self.footer.append(action_bar(*buttons))

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
        scroll = Gtk.ScrolledWindow(min_content_height=128, css_classes=["flatapk-logs"])
        scroll.set_child(text)
        expander = Gtk.Expander(label=_("Show details"))
        content = vertical(8)
        content.set_margin_top(8)
        content.append(scroll)
        copy = button(_("_Copy log"), lambda: self.get_clipboard().set("\n".join(self.log)), "flat")
        copy.set_halign(Gtk.Align.END)
        content.append(copy)
        expander.set_child(content)
        self.body.append(expander)

    def shell(self, heading, description, icon_name):
        row = Gtk.Box(spacing=12, css_classes=["flatapk-surface", "flatapk-hero"])
        icon = Gtk.Image.new_from_icon_name(icon_name)
        icon.set_pixel_size(32)
        tile = Gtk.Box(valign=Gtk.Align.CENTER, css_classes=["flatapk-icon-tile"])
        tile.set_hexpand(False)
        icon.set_hexpand(True)
        tile.append(icon)
        row.append(tile)
        words = vertical(4)
        words.set_valign(Gtk.Align.CENTER)
        words.set_hexpand(True)
        words.append(label(heading, "title-2"))
        if description:
            words.append(label(description, "dim-label"))
        row.append(words)
        self.body.append(row)
        return icon

    def signature_notice(self, verified):
        text = (_("Signature verified with a trusted key.") if verified else
                _("Signature could not be verified. Install only if you trust the source."))
        icon = "emblem-ok-symbolic" if verified else "dialog-warning-symbolic"
        classes = ["flatapk-notice", "success-note"] if verified else ["flatapk-notice"]
        notice = Gtk.Box(spacing=8, css_classes=classes)
        image = Gtk.Image.new_from_icon_name(icon)
        image.add_css_class("success" if verified else "warning")
        notice.append(image)
        notice.append(label(text))
        self.body.append(notice)

    def open_package(self, path):
        self.reset()
        self.shell(_("Open package"), path.name, "package-x-generic-symbolic")
        spinner = Gtk.Spinner(spinning=True, halign=Gtk.Align.CENTER)
        self.body.append(spinner)
        self.run_job(lambda: self.installer.probe(path), self.confirm)
        self.present()

    def runtime_controls(self, runtime="wine", proton_path=""):
        proton = Path(runtime).name == "proton" or runtime == "proton"
        runtime_box = vertical(6)
        choices = Gtk.DropDown.new_from_strings([_("Wine"), _("Proton")])
        choices.set_selected(1 if proton else 0)
        path_box = Gtk.Box(spacing=6)
        path = Gtk.Entry(placeholder_text=_("Path to the Proton executable"), hexpand=True)
        path.set_text(proton_path or (runtime if proton and Path(runtime).is_absolute() else ""))
        path.set_sensitive(proton)
        choices.connect("notify::selected", lambda dropdown, *_: path.set_sensitive(
            dropdown.get_selected() == 1))
        choose = button(_("Choose"), lambda: self.select_proton(path), "flat")
        path_box.append(path)
        path_box.append(choose)
        runtime_box.append(choices)
        runtime_box.append(path_box)
        return runtime_box, choices, path

    def select_proton(self, entry):
        dialog = Gtk.FileDialog(title=_("Choose the Proton executable"))

        def opened(dialog, result):
            try:
                file = dialog.open_finish(result)
            except GLib.Error:
                return
            if file and file.get_path():
                entry.set_text(file.get_path())

        dialog.open(self, None, opened)

    @staticmethod
    def selected_runtime(choices, path):
        return "wine" if choices.get_selected() == 0 else path.get_text().strip() or "proton"

    def confirm(self, result):
        package, plan = result
        self.reset()
        if self.compact:
            self.set_default_size(466, 434 if plan.method == "wine" else 370)
        icon = self.shell(package.name, package.version or package.path.name,
                          "package-x-generic-symbolic")
        if package.icon:
            try:
                stream = Gio.MemoryInputStream.new_from_bytes(GLib.Bytes.new(package.icon))
                pixbuf = GdkPixbuf.Pixbuf.new_from_stream_at_scale(stream, 42, 42, True, None)
                icon.set_from_pixbuf(pixbuf)
            except GLib.Error:
                pass
        if package.description:
            self.body.append(label(package.description))
        size = GLib.format_size(package.fingerprint[2])
        architecture = package.architecture or _("Unknown architecture")
        facts = Gtk.Box(spacing=6)
        for value in (package.kind.upper(), size, architecture):
            facts.append(label(value, "flatapk-pill"))
        self.body.append(facts)
        method = methods()[plan.method]
        if plan.family and plan.method == "toolbox":
            method += f" · {plan.family.capitalize()}"
        info = vertical(4)
        info.append(label(method, "heading"))
        origin = label(str(package.path), "dim-label")
        origin.set_selectable(True)
        origin.set_lines(2)
        origin.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        info.append(origin)
        self.body.append(info)
        self.signature_notice(package.signature == "verified")
        if plan.method == "native":
            self.body.append(label(_("The system will ask you to authorize this installation with polkit."), "dim-label"))
        elif plan.method == "toolbox":
            self.body.append(label(_("The program can access your home folder."), "dim-label"))
        if plan.method == "wine":
            self.body.append(label(_(
                "Windows programs can access your home folder. If needed, Packlift downloads Wine from Flathub for your account."),
                "dim-label"))
        if plan.multilib:
            self.body.append(label(_("Extra 32-bit components may be downloaded.")))
        preferences = self.installer.preferences()
        runtime_box, runtime_choice, proton_path = self.runtime_controls(
            preferences["runtime"], preferences["proton_path"])
        options = Gtk.Expander(label=_("Advanced options"))
        advanced = vertical(8)
        if package.requires:
            advanced.append(label(_("Dependencies") + "\n" + ", ".join(package.requires)))
        if plan.method == "wine":
            advanced.append(label(_("Windows compatibility runtime"), "heading"))
            advanced.append(runtime_box)
        if package.requires or plan.method == "wine":
            options.set_child(advanced)
            self.body.append(options)
        def install_package():
            selected_runtime = self.selected_runtime(runtime_choice, proton_path)
            self.begin(_("Installing {}").format(package.name), lambda: self.installer.install(
                package, plan, self.step, self.select_executable, selected_runtime))

        def add_game():
            selected_runtime = self.selected_runtime(runtime_choice, proton_path)
            self.begin(_("Adding {} to the menu").format(package.name), lambda: self.installer.add_game(
                package, self.step, selected_runtime))

        def choose_windows_action():
            dialog = Adw.AlertDialog(heading=_("How do you want to use this .exe?"),
                                     body=_("Choose Add to menu for a game that is already extracted. Choose Install Windows program to run a setup file."))
            dialog.add_response("cancel", _("_Cancel"))
            dialog.add_response("install", _("Install Windows program"))
            dialog.add_response("game", _("Add game to menu"))
            dialog.set_default_response("game")
            dialog.set_close_response("cancel")
            dialog.set_response_appearance("game", Adw.ResponseAppearance.SUGGESTED)
            dialog.connect("response::install", lambda *_: install_package())
            dialog.connect("response::game", lambda *_: add_game())
            dialog.present(self)

        actions = [button(_("_Cancel"), self.close, "flat")]
        if package.kind == "exe":
            actions.append(button(_("_Add game to menu"), add_game))
        install_button = button(_("_Install"), choose_windows_action if package.kind == "exe"
                               else install_package, "suggested-action")
        self.set_actions(*actions, install_button)
        self.set_default_widget(install_button)
        install_button.grab_focus()

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
        progress_card = vertical(8)
        progress_card.add_css_class("flatapk-surface")
        progress_card.append(self.status)
        progress_card.append(self.progress)
        self.body.append(progress_card)
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
        if app and app.get("game_shortcut"):
            self.manage()
            return
        if error:
            message = _(str(error)) if isinstance(error, FlatapkError) else _(
                "Packlift could not complete this operation.")
            self.append_log(str(error))
            self.shell(_("Operation failed"), message, "dialog-error-symbolic")
            self.body.append(label(_("Check the details for the failing step and try again."), "dim-label"))
        else:
            self.shell(_("Done"), app["name"] if app else "", "emblem-ok-symbolic")
        self.details()
        actions = [button(_("_Close"), self.close)]
        if app and app["exports"]:
            actions.append(button(_("_Open program"), lambda: self.launch(app), "suggested-action"))
        self.set_actions(*actions)

    def launch(self, app):
        export = app["exports"][0]
        appimage = (app["method"] == "local" and not export["terminal"]
                    and (Path(app["directory"]) / "program.AppImage").is_file())
        if app.get("game_shortcut") or appimage:
            threading.Thread(target=self.watch_program, args=(app,), daemon=True).start()
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

    def watch_program(self, app):
        try:
            self.installer.observe_launch(app)
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
                                 body=_("Choose Wine or Proton for this program and edit its launch settings."))
        content = vertical(10)
        runtime_box, runtime_choice, proton_path = self.runtime_controls(
            app["runtime"], self.installer.preferences()["proton_path"])
        content.append(label(_("Windows compatibility runtime"), "heading"))
        content.append(runtime_box)
        content.append(label(_("Enter one NAME=value per line, including WINEDLLOVERRIDES if needed."), "heading"))
        entry = Gtk.TextView(monospace=True, height_request=96)
        buffer = entry.get_buffer()
        buffer.set_text("\n".join(f"{key}={value}" for key, value in app.get("environment", {}).items()))
        content.append(entry)
        dialog.set_extra_child(content)
        dialog.add_response("cancel", _("_Cancel"))
        dialog.add_response("save", _("_Save"))
        dialog.set_close_response("cancel")
        def save(*_):
            text = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
            runtime = self.selected_runtime(runtime_choice, proton_path)
            self.run_job(lambda: self.installer.configure(app, text, runtime), lambda _: self.manage())

        dialog.connect("response::save", save)
        dialog.present(self)

    def app_settings(self):
        preferences = self.installer.preferences()
        dialog = Adw.AlertDialog(heading=_("Packlift settings"),
                                 body=_("Choose the default runtime for new Windows games and programs."))
        content = vertical(10)
        runtime_box, runtime_choice, proton_path = self.runtime_controls(
            preferences["runtime"], preferences["proton_path"])
        content.append(label(_("Default Windows runtime"), "heading"))
        content.append(runtime_box)
        dialog.set_extra_child(content)
        dialog.add_response("cancel", _("_Cancel"))
        dialog.add_response("save", _("_Save"))
        dialog.set_close_response("cancel")

        def save(*_):
            runtime = "wine" if runtime_choice.get_selected() == 0 else "proton"
            path = proton_path.get_text().strip()
            self.run_job(lambda: self.installer.save_preferences(runtime, path),
                         lambda _: self.manage())

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
        apps = self.installer.registry.apps()
        open_package = button(_("_Open package"), self.file_dialog, "suggested-action")
        self.header.pack_start(open_package)
        settings = Gtk.Button.new_from_icon_name("preferences-system-symbolic")
        settings.connect("clicked", lambda *_: self.app_settings())
        settings.add_css_class("flat")
        settings.set_tooltip_text(_("Packlift settings"))
        self.header.pack_end(settings)
        tools = [(_("Clean unused Toolbox environments"), lambda: self.confirm_action(
            _("Clean unused Toolbox environments?"),
            _("Packlift removes its Toolbox containers that have no registered programs."),
            lambda: self.begin(_("Cleaning environments"), lambda: self.installer.clean(self.step))))]
        if any(app["method"] == "wine" and not app.get("game_shortcut") for app in apps):
            tools.append((_("Repair Wine prefix"), lambda: self.confirm_action(
                _("Rebuild the shared Wine prefix?"),
                _("This runs every saved installer again. Settings and files inside the old prefix can be lost."),
                lambda: self.begin(_("Repairing Wine prefix"), lambda: self.installer.repair_wine(self.step)),
                destructive=True)))
        if (self.installer.data / "wine/undo/app-id").is_file():
            tools.append((_("Undo last Wine installation"), lambda: self.confirm_action(
                _("Restore the last Wine snapshot?"),
                _("This removes the most recently installed Windows program and restores the saved prefix."),
                lambda: self.begin(_("Restoring Wine prefix"), lambda: self.installer.undo_wine(self.step)),
                destructive=True)))
        overflow = menu_button(_("Maintenance options"), tools)
        self.header.pack_end(overflow)
        self.header_controls = [open_package, settings, overflow]

        if not apps:
            self.body.append(Adw.StatusPage(
                title=_("No programs installed"),
                description=_("Open a package file to install your first program."),
                icon_name="package-x-generic-symbolic", css_classes=["compact"]))
            return
        self.shell(_("Installed programs"), "", "view-grid-symbolic")
        group = Adw.PreferencesGroup()
        for app in sorted(apps, key=lambda item: item["name"].casefold()):
            group.add(self.program_row(app))
        self.body.append(group)

    def program_row(self, app):
        if app.get("game_shortcut"):
            runtime_name = _("Proton") if Path(app["runtime"]).name == "proton" else _("Wine")
            subtitle = _("Game shortcut") + " · " + runtime_name
        else:
            subtitle = methods()[app["method"]]
        row = Adw.ActionRow(title=GLib.markup_escape_text(app["name"]),
                            subtitle=GLib.markup_escape_text(subtitle), css_classes=["flatapk-program"])
        icon_name = app["exports"][0]["icon"] if app["exports"] else DEFAULT_APP_ICON
        row.add_prefix(installed_icon(icon_name))
        open_program = button(_("_Open"), lambda: self.launch(app), "flat")
        open_program.set_valign(Gtk.Align.CENTER)
        row.add_suffix(open_program)
        row.set_activatable_widget(open_program)
        options = []
        if app["method"] == "wine":
            options.append((_("Settings"), lambda: self.settings(app)))
        if app.get("game_shortcut"):
            options.append((_("Remove shortcut"), lambda: self.confirm_action(
                _("Remove the shortcut for {}?").format(app["name"]),
                _("The game files stay in their original folder."),
                lambda: self.begin(_("Removing shortcut"), lambda: self.installer.uninstall(app, self.step)),
                destructive=True, response_text=_("_Remove shortcut"))))
        else:
            options.append((_("Uninstall"), lambda: self.confirm_action(
                _("Uninstall {}?").format(app["name"]), _("This removes the program and its shortcuts."),
                lambda: self.begin(_("Uninstalling"), lambda: self.installer.uninstall(app, self.step)),
                destructive=True, response_text=_("_Uninstall"))))
        row.add_suffix(menu_button(_("Program options"), options))
        return row


class Application(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID,
                         flags=Gio.ApplicationFlags.HANDLES_OPEN | Gio.ApplicationFlags.NON_UNIQUE)

    def do_startup(self):
        Adw.Application.do_startup(self)
        provider = Gtk.CssProvider()
        provider.load_from_path(str(Path(__file__).with_name("style.css")))
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

    def do_activate(self):
        window = Window(self, compact=False)
        if window.installer.appimages_missing_icons() or window.installer.games_missing_icons():
            window.shell(_("Updating application icons"), "", "image-x-generic-symbolic")
            window.body.append(Gtk.Spinner(spinning=True, halign=Gtk.Align.CENTER))
            def refresh_icons():
                window.installer.refresh_appimage_icons()
                window.installer.refresh_game_icons()

            window.run_job(refresh_icons, lambda _: window.manage())
        else:
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
