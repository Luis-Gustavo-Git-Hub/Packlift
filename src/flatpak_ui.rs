use std::{cell::Cell, rc::Rc, sync::Arc};

use adw::prelude::*;
use packlift::{
    core::Result,
    flatpak::{Access, BusAccess, Change, FileAccess, FlatpakTarget, Permissions},
};
use serde_json::Value;

use crate::{home_label, i18n::tr, installer, text_button};

struct PermissionSwitch {
    kind: Access,
    key: &'static str,
    row: adw::SwitchRow,
    original: Cell<bool>,
}

impl PermissionSwitch {
    fn load(&self, permissions: &str) {
        let active = self.kind.enabled(permissions, self.key);
        self.row.set_active(active);
        self.original.set(active);
    }

    fn change(&self) -> Option<Change> {
        let active = self.row.is_active();
        (active != self.original.get()).then_some(Change::Toggle(self.kind, self.key, active))
    }
}

pub(super) fn editor(window: &adw::ApplicationWindow, record: &Value) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let status = home_label(tr("Loading permissions…"), "dim-label");
    status.set_selectable(true);
    panel.append(&status);

    let Some(reference) = record["target"].as_str() else {
        set_status(&status, tr("Could not load permissions."), true);
        return panel;
    };
    let target = Arc::new(FlatpakTarget {
        reference: reference.into(),
        installation: record["flatpak_scope"].as_str().unwrap_or("user").into(),
    });
    let retry = text_button(tr("Try again"));
    retry.set_halign(gtk::Align::Start);
    retry.set_visible(false);
    panel.append(&retry);

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 16);
    controls.set_visible(false);
    panel.append(&controls);
    let mut switches = Vec::new();
    for (title, entries) in [
        (
            "Network and display",
            &[
                (Access::Share, "network", "Network access"),
                (Access::Share, "ipc", "Shared memory (IPC)"),
                (Access::Socket, "wayland", "Wayland"),
                (Access::Socket, "x11", "X11"),
                (Access::Socket, "fallback-x11", "Fallback X11"),
            ][..],
        ),
        (
            "Audio and devices",
            &[
                (Access::Socket, "pulseaudio", "Audio"),
                (Access::Device, "dri", "Graphics acceleration"),
                (Access::Device, "input", "Input devices"),
                (Access::Device, "usb", "USB devices"),
                (Access::Device, "all", "All devices"),
                (Access::Device, "kvm", "Virtualization (KVM)"),
            ][..],
        ),
        (
            "System services",
            &[
                (Access::Socket, "cups", "Printing"),
                (Access::Socket, "ssh-auth", "SSH agent"),
                (Access::Socket, "gpg-agent", "GPG agent"),
                (Access::Socket, "pcsc", "Smart cards"),
                (Access::Socket, "session-bus", "Unrestricted session bus"),
                (Access::Socket, "system-bus", "Unrestricted system bus"),
                (Access::Feature, "bluetooth", "Bluetooth"),
            ][..],
        ),
    ] {
        let group = adw::PreferencesGroup::builder().title(tr(title)).build();
        for &(kind, key, label) in entries {
            let row = adw::SwitchRow::builder().title(tr(label)).build();
            let switch = PermissionSwitch {
                kind,
                key,
                row,
                original: Cell::new(false),
            };
            group.add(&switch.row);
            switches.push(switch);
        }
        controls.append(&group);
    }

    let extra = adw::PreferencesGroup::new();
    let advanced = rule_group(&extra, tr("Advanced"));
    let rules = adw::PreferencesGroup::builder()
        .title(tr("Custom rules"))
        .build();
    rules.set_margin_top(8);
    rules.set_margin_bottom(8);
    rules.set_margin_start(12);
    rules.set_margin_end(12);
    advanced.add_row(&rules);
    let filesystem = rule_group(&rules, tr("Files and folders"));
    let location = choice(
        &filesystem,
        tr("Location"),
        &[
            tr("Custom path"),
            tr("Home folder"),
            tr("Host files"),
            tr("Downloads"),
        ],
    );
    let path = entry(&filesystem, tr("Path"), "/mnt/Games");
    let location_path = path.clone();
    location.connect_selected_notify(move |choice| {
        let preset = ["", "home", "host", "xdg-download"][choice.selected().min(3) as usize];
        location_path.set_text(preset);
        location_path.set_editable(preset.is_empty());
    });
    let file_modes = choice(
        &filesystem,
        tr("File access"),
        &[
            tr("Read and write"),
            tr("Read only"),
            tr("Create if missing"),
            tr("Block"),
        ],
    );

    let bus = rule_group(&rules, tr("D-Bus services"));
    let service = entry(&bus, tr("Service name"), "org.example.Service");
    let bus_scope = choice(&bus, tr("Bus"), &[tr("Session bus"), tr("System bus")]);
    let bus_modes = choice(
        &bus,
        tr("Service access"),
        &[tr("Talk"), tr("Own name"), tr("Block")],
    );

    let environment = rule_group(&rules, tr("Environment variables"));
    let variable = entry(&environment, tr("Variable name"), "EXAMPLE_VAR");
    let value = entry(&environment, tr("Value"), "");
    let env_modes = choice(
        &environment,
        tr("Variable action"),
        &[tr("Set value"), tr("Unset variable")],
    );
    let value_entry = value.clone();
    env_modes.connect_selected_notify(move |mode| value_entry.set_sensitive(mode.selected() == 0));
    let reports = rule_group(&extra, tr("Details"));
    let effective_group = adw::PreferencesGroup::builder()
        .title(tr("Effective permissions"))
        .build();
    let effective = report(&effective_group);
    let portal_group = adw::PreferencesGroup::builder()
        .title(tr("Portal permissions"))
        .description(tr(
            "These permissions were granted through desktop prompts.",
        ))
        .build();
    let portal = report(&portal_group);
    let reset_portal = text_button(tr("Reset portal permissions…"));
    reset_portal.set_halign(gtk::Align::Start);
    reset_portal.set_margin_top(10);
    portal_group.add(&reset_portal);
    let report_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    report_content.set_margin_top(8);
    report_content.set_margin_bottom(8);
    report_content.set_margin_start(12);
    report_content.set_margin_end(12);
    report_content.append(&effective_group);
    report_content.append(&portal_group);
    reports.add_row(&report_content);
    controls.append(&extra);

    controls.append(&home_label(
        tr("Changes apply to all installations of this app for your account. Restart the app to apply them."),
        "dim-label",
    ));
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let reset = text_button(tr("Restore defaults…"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let save = text_button(tr("Save"));
    save.add_css_class("suggested-action");
    actions.append(&reset);
    actions.append(&spacer);
    actions.append(&save);
    controls.append(&actions);

    let switches = Rc::new(switches);
    let update = {
        let switches = switches.clone();
        let entries = [
            path.clone(),
            service.clone(),
            variable.clone(),
            value.clone(),
        ];
        let reset_portal = reset_portal.downgrade();
        move |permissions: Permissions, reload_edits: bool| {
            if reload_edits {
                for switch in &*switches {
                    switch.load(&permissions.effective);
                }
                location.set_selected(0);
                for entry in &entries {
                    entry.set_text("");
                }
            }
            effective.set_text(if permissions.effective.trim().is_empty() {
                tr("No permissions reported.")
            } else {
                permissions.effective.trim()
            });
            if let Some(reset_portal) = reset_portal.upgrade() {
                reset_portal.set_sensitive(
                    permissions
                        .portal
                        .as_ref()
                        .is_some_and(|text| !text.trim().is_empty()),
                );
            }
            portal.set_text(match permissions.portal.as_deref().map(str::trim) {
                Some("") => tr("No portal permissions saved."),
                Some(text) => text,
                None => tr("The desktop permission store is unavailable."),
            });
        }
    };
    let editor = Rc::new(Editor {
        target,
        controls: controls.downgrade(),
        status: status.downgrade(),
        retry: retry.downgrade(),
        update: Box::new(update),
        busy: Cell::new(false),
    });
    let start = editor.clone();
    panel.connect_map(move |_| {
        if let Some(controls) = start.controls.upgrade() {
            if !controls.is_visible() && !start.retry.upgrade().is_some_and(|row| row.is_visible())
            {
                start.run(Action::Load);
            }
        }
    });
    let reload = editor.clone();
    retry.connect_clicked(move |_| reload.run(Action::Load));

    let save_editor = editor.clone();
    save.connect_clicked(move |_| {
        let mut changes: Vec<_> = switches
            .iter()
            .filter_map(PermissionSwitch::change)
            .collect();
        if !path.text().trim().is_empty() {
            let mode = match file_modes.selected() {
                1 => FileAccess::ReadOnly,
                2 => FileAccess::Create,
                3 => FileAccess::Deny,
                _ => FileAccess::ReadWrite,
            };
            changes.push(Change::Filesystem(path.text().trim().into(), mode));
        }
        if !service.text().trim().is_empty() {
            let level = match bus_modes.selected() {
                1 => BusAccess::Own,
                2 => BusAccess::Deny,
                _ => BusAccess::Talk,
            };
            changes.push(Change::Bus {
                system: bus_scope.selected() == 1,
                name: service.text().trim().into(),
                level,
            });
        }
        if !variable.text().trim().is_empty() || !value.text().is_empty() {
            changes.push(Change::Environment {
                name: variable.text().trim().into(),
                value: (env_modes.selected() == 0).then(|| value.text().into()),
            });
        }
        if changes.is_empty() {
            if let Some(status) = save_editor.status.upgrade() {
                set_status(&status, tr("No changes to save."), false);
            }
        } else {
            save_editor.run(Action::Save(changes));
        }
    });

    for (button, portal_only, question, detail) in [
        (
            reset,
            false,
            tr("Restore default permissions?"),
            tr("Remove all custom sandbox rules for this app from your account."),
        ),
        (
            reset_portal,
            true,
            tr("Reset portal permissions?"),
            tr("The app may ask again for permissions granted through desktop prompts."),
        ),
    ] {
        let parent = window.downgrade();
        let panel = panel.downgrade();
        let editor = editor.clone();
        button.connect_clicked(move |_| {
            let Some(parent) = parent.upgrade() else {
                return;
            };
            let dialog = adw::MessageDialog::new(Some(&parent), Some(question), Some(detail));
            dialog.add_response("cancel", tr("Cancel"));
            dialog.add_response("reset", tr("Reset"));
            dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let editor = editor.clone();
            let panel = panel.clone();
            dialog.connect_response(Some("reset"), move |_, _| {
                if panel.upgrade().is_some_and(|panel| panel.is_mapped()) {
                    editor.run(Action::Reset { portal_only });
                }
            });
            dialog.present();
        });
    }
    panel
}

enum Action {
    Load,
    Save(Vec<Change>),
    Reset { portal_only: bool },
}

struct Editor {
    target: Arc<FlatpakTarget>,
    controls: glib::WeakRef<gtk::Box>,
    status: glib::WeakRef<gtk::Label>,
    retry: glib::WeakRef<gtk::Button>,
    update: Box<dyn Fn(Permissions, bool)>,
    busy: Cell<bool>,
}

impl Editor {
    fn run(self: &Rc<Self>, action: Action) {
        let (Some(controls), Some(status), Some(retry)) = (
            self.controls.upgrade(),
            self.status.upgrade(),
            self.retry.upgrade(),
        ) else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        controls.set_sensitive(false);
        retry.set_visible(false);
        status.set_visible(true);
        let (pending, done, reload_edits) = match &action {
            Action::Load => (tr("Loading permissions…"), "", true),
            Action::Save(_) => (
                tr("Working…"),
                tr("Permissions saved. Restart the app to apply them."),
                true,
            ),
            Action::Reset { portal_only } => {
                (tr("Working…"), tr("Permissions reset."), !portal_only)
            }
        };
        set_status(&status, pending, false);
        let editor = self.clone();
        let target = self.target.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(move || -> Result<Result<Permissions>> {
                let runner = installer()?.runner;
                match action {
                    Action::Load => {}
                    Action::Save(changes) => target.apply(&runner, &changes)?,
                    Action::Reset { portal_only } => target.reset(&runner, portal_only)?,
                }
                Ok(target.permissions(&runner))
            })
            .await;
            editor.busy.set(false);
            let (Some(controls), Some(status), Some(retry)) = (
                editor.controls.upgrade(),
                editor.status.upgrade(),
                editor.retry.upgrade(),
            ) else {
                return;
            };
            match result {
                Ok(Ok(Ok(permissions))) => {
                    (editor.update)(permissions, reload_edits);
                    controls.set_sensitive(true);
                    controls.set_visible(true);
                    status.set_visible(!done.is_empty());
                    if !done.is_empty() {
                        set_status(&status, done, false);
                    }
                }
                Ok(Ok(Err(error))) => {
                    let message = if done.is_empty() {
                        error.to_string()
                    } else {
                        format!("{done} {error}")
                    };
                    set_status(&status, &message, true);
                    retry.set_visible(true);
                }
                Ok(Err(error)) => {
                    set_status(&status, &error.to_string(), true);
                    controls.set_sensitive(!done.is_empty());
                    retry.set_visible(done.is_empty());
                }
                Err(_) => {
                    set_status(&status, tr("The operation ended unexpectedly."), true);
                    retry.set_visible(true);
                }
            }
        });
    }
}

fn rule_group(group: &adw::PreferencesGroup, title: &str) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title(title).build();
    group.add(&row);
    row
}

fn entry(row: &adw::ExpanderRow, title: &str, hint: &str) -> gtk::Entry {
    let line = adw::ActionRow::builder().title(title).build();
    let field = gtk::Entry::builder()
        .placeholder_text(hint)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    field.update_property(&[gtk::accessible::Property::Label(title)]);
    line.add_suffix(&field);
    row.add_row(&line);
    field
}

fn choice(row: &adw::ExpanderRow, title: &str, options: &[&str]) -> adw::ComboRow {
    let choice = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(options))
        .build();
    row.add_row(&choice);
    choice
}

fn report(group: &adw::PreferencesGroup) -> gtk::Label {
    let label = home_label("", "monospace");
    label.set_selectable(true);
    label.set_margin_start(12);
    label.set_margin_end(12);
    label.set_margin_top(10);
    label.set_margin_bottom(10);
    let frame = gtk::Frame::builder().child(&label).build();
    frame.add_css_class("card");
    group.add(&frame);
    label
}

fn set_status(label: &gtk::Label, message: &str, error: bool) {
    label.set_text(message);
    if error {
        label.add_css_class("error");
    } else {
        label.remove_css_class("error");
    }
    label.announce(message, gtk::AccessibleAnnouncementPriority::Medium);
}
