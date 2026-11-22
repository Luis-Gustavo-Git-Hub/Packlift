use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use packlift::flatpak::{Access, BusAccess, Change, FileAccess, FlatpakTarget, Permissions};
use serde_json::Value;

use crate::{home_label, i18n::tr, installer, show_error, text_button};

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

pub(super) fn show(parent: &adw::ApplicationWindow, record: &Value) {
    let Some(reference) = record["target"].as_str() else {
        return;
    };
    let target = Rc::new(FlatpakTarget {
        reference: reference.into(),
        installation: record["flatpak_scope"].as_str().unwrap_or("user").into(),
    });
    let manager = match installer().and_then(|manager| {
        target
            .permissions(&manager.runner)
            .map(|permissions| (manager, permissions))
    }) {
        Ok(value) => value,
        Err(error) => {
            show_error(parent, &error.to_string());
            return;
        }
    };
    let (manager, permissions) = manager;
    let runner = Rc::new(manager.runner);

    let window = adw::Window::builder()
        .title(tr("Flatpak permissions"))
        .transient_for(parent)
        .modal(true)
        .default_width(610)
        .default_height(640)
        .build();

    let stack = adw::ViewStack::new();
    let navigation = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Narrow)
        .build();
    let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_box.set_halign(gtk::Align::Center);
    nav_box.append(&navigation);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new(
        record["name"].as_str().unwrap_or(tr("Flatpak permissions")),
        tr("Flatpak permissions"),
    );
    header.set_title_widget(Some(&title));
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&nav_box);
    toolbar.set_content(Some(&stack));
    window.set_content(Some(&toolbar));

    let access_page = adw::PreferencesPage::new();
    let identity = adw::PreferencesGroup::builder()
        .title(tr("Application ID"))
        .description(reference.split('/').next().unwrap_or_default())
        .build();
    access_page.add(&identity);
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
            switch.load(&permissions.effective);
            group.add(&switch.row);
            switches.push(switch);
        }
        access_page.add(&group);
    }
    stack.add_titled_with_icon(
        &access_page,
        Some("access"),
        tr("Access"),
        "changes-prevent-symbolic",
    );

    let advanced = adw::PreferencesPage::new();
    let rules = adw::PreferencesGroup::builder()
        .title(tr("Custom rules"))
        .description(tr("Enter the path, service or variable you want to change. View current rules in Details."))
        .build();
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
    advanced.add(&rules);
    stack.add_titled_with_icon(
        &advanced,
        Some("advanced"),
        tr("Advanced"),
        "applications-engineering-symbolic",
    );

    let details = adw::PreferencesPage::new();
    let effective_group = adw::PreferencesGroup::builder()
        .title(tr("Effective permissions"))
        .build();
    let effective = report(&effective_group);
    details.add(&effective_group);
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
    details.add(&portal_group);
    stack.add_titled_with_icon(
        &details,
        Some("details"),
        tr("Details"),
        "dialog-information-symbolic",
    );

    let footer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    footer.set_margin_top(10);
    footer.set_margin_bottom(14);
    footer.set_margin_start(16);
    footer.set_margin_end(16);
    let status = home_label(
        tr("Changes apply to all installations of this app for your account. Restart the app to apply them."),
        "dim-label",
    );
    status.set_selectable(true);
    footer.append(&status);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let reset = text_button(tr("Restore defaults…"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let save = text_button(tr("Save"));
    save.add_css_class("suggested-action");
    actions.append(&reset);
    actions.append(&spacer);
    actions.append(&save);
    footer.append(&actions);
    toolbar.set_bottom_bar_style(adw::ToolbarStyle::RaisedBorder);
    toolbar.add_bottom_bar(&footer);

    let switches = Rc::new(switches);
    let update = Rc::new({
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
    });
    update(permissions, true);

    let save_target = target.clone();
    let save_runner = runner.clone();
    let save_status = status.clone();
    let save_update = update.clone();
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
            set_status(&save_status, tr("No changes to save."), false);
            return;
        }
        match save_target.apply(&save_runner, &changes) {
            Ok(()) => {
                set_status(
                    &save_status,
                    tr("Permissions saved. Restart the app to apply them."),
                    false,
                );
                if let Ok(permissions) = save_target.permissions(&save_runner) {
                    save_update(permissions, true);
                }
            }
            Err(error) => set_status(&save_status, &error.to_string(), true),
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
        let target = target.clone();
        let runner = runner.clone();
        let update = update.clone();
        let status = status.clone();
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
            let target = target.clone();
            let runner = runner.clone();
            let update = update.clone();
            let status = status.clone();
            dialog.connect_response(Some("reset"), move |_, _| {
                match target.reset(&runner, portal_only) {
                    Ok(()) => {
                        set_status(&status, tr("Permissions reset."), false);
                        if let Ok(permissions) = target.permissions(&runner) {
                            update(permissions, !portal_only);
                        }
                    }
                    Err(error) => set_status(&status, &error.to_string(), true),
                }
            });
            dialog.present();
        });
    }
    window.present();
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
