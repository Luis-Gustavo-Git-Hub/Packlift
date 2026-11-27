use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use adw::prelude::*;
use packlift::{core::PackliftError, installer::WindowsLaunchOptions};
use serde_json::Value;

use crate::{home_label, i18n::tr, installer, text_button};

pub(super) fn launch_settings(
    window: &adw::ApplicationWindow,
    record: &Value,
    saved: impl Fn(Value) + 'static,
) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let Some(id) = record["id"].as_str().map(str::to_owned) else {
        return panel;
    };
    let group = adw::PreferencesGroup::builder()
        .title(tr("Windows launch options"))
        .description(tr(
            "These settings also apply to the shortcut in your app menu.",
        ))
        .build();
    panel.append(&group);

    let runtime = adw::ComboRow::builder().title(tr("Runtime")).build();
    group.add(&runtime);
    let executable = record["exports"][0]["wine_args"][0]
        .as_str()
        .unwrap_or_default();
    if !executable.is_empty() {
        let path = adw::ActionRow::builder()
            .use_markup(false)
            .title(tr("Executable"))
            .subtitle(executable)
            .subtitle_lines(2)
            .build();
        path.set_tooltip_text(Some(executable));
        group.add(&path);
    }

    let directory = adw::EntryRow::builder()
        .title(tr("Working directory"))
        .build();
    directory.set_text(record["launch_directory"].as_str().unwrap_or_default());
    let browse = gtk::Button::from_icon_name("folder-open-symbolic");
    browse.add_css_class("flat");
    browse.set_valign(gtk::Align::Center);
    browse.set_tooltip_text(Some(tr("Choose working directory…")));
    browse.update_property(&[gtk::accessible::Property::Label(tr(
        "Choose working directory…",
    ))]);
    directory.add_suffix(&browse);
    group.add(&directory);
    let owner = window.downgrade();
    let target = directory.downgrade();
    browse.connect_clicked(move |_| {
        if let Some(window) = owner.upgrade() {
            let target = target.clone();
            gtk::FileDialog::builder()
                .title(tr("Choose working directory…"))
                .build()
                .select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                    if let (Some(row), Ok(folder)) = (target.upgrade(), result) {
                        if let Some(path) = folder.path() {
                            row.set_text(&path.to_string_lossy());
                        }
                    }
                });
        }
    });

    let arguments = adw::EntryRow::builder()
        .title(tr("Launch arguments"))
        .build();
    arguments.set_text(record["launch_arguments"].as_str().unwrap_or_default());
    group.add(&arguments);

    let options = &record["wine_options"];
    let disable_dxvk = adw::SwitchRow::builder()
        .title(tr("Disable DXVK (Proton)"))
        .active(options["disable_dxvk"].as_bool().unwrap_or(false))
        .build();
    let disable_vkd3d = adw::SwitchRow::builder()
        .title(tr("Disable VKD3D-Proton (Proton)"))
        .active(options["disable_vkd3d"].as_bool().unwrap_or(false))
        .build();
    let disable_esync = adw::SwitchRow::builder()
        .title(tr("Disable Esync (Proton)"))
        .active(options["disable_esync"].as_bool().unwrap_or(false))
        .build();
    let disable_fsync = adw::SwitchRow::builder()
        .title(tr("Disable Fsync (Proton)"))
        .active(options["disable_fsync"].as_bool().unwrap_or(false))
        .build();
    for row in [
        &disable_dxvk,
        &disable_vkd3d,
        &disable_esync,
        &disable_fsync,
    ] {
        group.add(row);
    }
    let compatibility_help = home_label(
        tr("These compatibility switches are used by Proton. They do not change the selected runtime."),
        "dim-label",
    );
    panel.append(&compatibility_help);
    let environment_label = home_label(tr("Environment variables"), "heading");
    panel.append(&environment_label);
    let environment = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .monospace(true)
        .left_margin(10)
        .right_margin(10)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    let environment_buffer = environment.buffer();
    if let Some(entries) = record["launch_environment"].as_object() {
        let content = entries
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| format!("{key}={value}")))
            .collect::<Vec<_>>()
            .join("\n");
        environment_buffer.set_text(&content);
    }
    let environment_scroll = gtk::ScrolledWindow::builder()
        .child(&environment)
        .min_content_height(105)
        .max_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    environment_scroll.add_css_class("card");
    panel.append(&environment_scroll);
    let environment_help = home_label(
        tr("Write one NAME=VALUE per line, for example DXVK_HUD=fps. These variables apply only to this game."),
        "dim-label",
    );
    panel.append(&environment_help);
    let default_directory = record["working_directory"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| {
            PathBuf::from(executable)
                .parent()
                .map(std::path::Path::to_owned)
        });
    let help = home_label(
        tr("Leave the working directory blank to use the default. Put arguments that contain spaces in quotes."),
        "dim-label",
    );
    if let Some(path) = default_directory {
        help.set_tooltip_text(Some(&path.to_string_lossy()));
    }
    panel.append(&help);

    let status = home_label(tr("Loading runtimes…"), "dim-label");
    panel.append(&status);
    let save = text_button(tr("Save"));
    save.set_halign(gtk::Align::Start);
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    panel.append(&save);

    let versions = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let started = Cell::new(false);
    let load_versions = versions.clone();
    let use_proton = record["runtime"] == "proton";
    let current_path = record["proton_path"].as_str().map(PathBuf::from);
    let (runtime_ref, status_ref, save_ref) =
        (runtime.downgrade(), status.downgrade(), save.downgrade());
    panel.connect_map(move |_| {
        if started.replace(true) {
            return;
        }
        let (runtime, status, save) = (runtime_ref.clone(), status_ref.clone(), save_ref.clone());
        let (versions, current_path) = (load_versions.clone(), current_path.clone());
        glib::MainContext::default().spawn_local(async move {
            let installed = gio::spawn_blocking(|| {
                installer().map(|manager| manager.installed_proton_versions())
            })
            .await;
            let (Some(runtime), Some(status), Some(save)) =
                (runtime.upgrade(), status.upgrade(), save.upgrade())
            else {
                return;
            };
            let mut available = match installed {
                Ok(Ok(available)) => available,
                _ => {
                    status.set_text(tr("Could not load Windows runtimes."));
                    return;
                }
            };
            let missing = current_path
                .as_ref()
                .is_some_and(|path| !available.contains(path));
            // Keep the saved version selected even if it has been removed.
            if let Some(path) = current_path.clone().filter(|_| missing) {
                available.push(path);
            }
            let mut labels = vec!["Wine".to_owned(), tr("Proton (automatic)").to_owned()];
            labels.extend(available.iter().map(|path| {
                format!(
                    "Proton · {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            }));
            let selected = if use_proton {
                if missing {
                    (available.len() + 1) as u32
                } else {
                    current_path
                        .as_ref()
                        .and_then(|path| available.iter().position(|version| version == path))
                        .map_or(1, |index| (index + 2) as u32)
                }
            } else {
                0
            };
            let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
            runtime.set_model(Some(&gtk::StringList::new(&labels)));
            runtime.set_selected(selected);
            *versions.borrow_mut() = available;
            if missing {
                status.set_text(tr(
                    "The previously selected Proton version is no longer installed.",
                ));
            } else {
                status.set_text("");
            }
            save.set_sensitive(true);
        });
    });

    let saved = Rc::new(saved);
    let application = window.application().map(|app| app.downgrade());
    let (runtime, directory, arguments, status) = (
        runtime.downgrade(),
        directory.downgrade(),
        arguments.downgrade(),
        status.downgrade(),
    );
    let (disable_dxvk, disable_vkd3d, disable_esync, disable_fsync) = (
        disable_dxvk.downgrade(),
        disable_vkd3d.downgrade(),
        disable_esync.downgrade(),
        disable_fsync.downgrade(),
    );
    let environment_buffer = environment_buffer.clone();
    let fields = panel.downgrade();
    save.connect_clicked(move |button| {
        let (Some(runtime), Some(directory), Some(arguments), Some(label), Some(group), Some(disable_dxvk), Some(disable_vkd3d), Some(disable_esync), Some(disable_fsync)) =
            (runtime.upgrade(), directory.upgrade(), arguments.upgrade(), status.upgrade(), fields.upgrade(), disable_dxvk.upgrade(), disable_vkd3d.upgrade(), disable_esync.upgrade(), disable_fsync.upgrade())
        else { return; };
        let selection = runtime.selected();
        let runtime = if selection == 0 { "wine" } else { "proton" };
        let proton_path = selection.checked_sub(2).and_then(|index| versions.borrow().get(index as usize).cloned());
        let path = directory.text();
        let options = WindowsLaunchOptions {
            working_directory: (!path.is_empty()).then(|| PathBuf::from(path.as_str())),
            arguments: arguments.text().to_string(),
            environment: environment_buffer.text(
                &environment_buffer.start_iter(),
                &environment_buffer.end_iter(),
                false,
            ).to_string(),
            disable_dxvk: disable_dxvk.is_active(),
            disable_vkd3d: disable_vkd3d.is_active(),
            disable_esync: disable_esync.is_active(),
            disable_fsync: disable_fsync.is_active(),
        };
        button.set_sensitive(false);
        group.set_sensitive(false);
        label.set_text(tr("Saving launch settings…"));
        label.remove_css_class("error");
        let (button, label, fields) = (button.downgrade(), status.clone(), fields.clone());
        let (id, saved) = (id.clone(), saved.clone());
        let hold = application.as_ref().and_then(glib::WeakRef::upgrade).map(|app| app.hold());
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let result = gio::spawn_blocking(move || {
                installer()?.set_windows_launch_options(&id, runtime, proton_path.as_deref(), options)
            }).await.unwrap_or_else(|_| {
                Err(PackliftError::Message(tr("The operation ended unexpectedly.").into()))
            });
            if let Ok(record) = &result {
                saved(record.clone());
            }
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
            if let Some(group) = fields.upgrade() {
                group.set_sensitive(true);
            }
            if let Some(label) = label.upgrade() {
                match result {
                    Ok(_) => label.set_text(tr("Launch settings saved.")),
                    Err(error) => {
                        let message = error.to_string();
                        label.set_text(match message.as_str() {
                            "Choose an existing folder for the working directory." => tr("Choose an existing folder for the working directory."),
                            "Check the quotes and characters in the launch arguments." => tr("Check the quotes and characters in the launch arguments."),
                            "That Proton version is no longer available. Choose another runtime in Settings." => tr("That Proton version is no longer available. Choose another runtime in Settings."),
                            _ => &message,
                        });
                        label.add_css_class("error");
                    }
                }
            }
        });
    });
    panel
}
