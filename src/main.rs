mod i18n;

use adw::prelude::*;
use i18n::tr;
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc, thread, time::Duration};

use packlift::{
    core::{Package, PackliftError, Result},
    installer::{windows_prefix_drive, Installer},
    system::{host_access_available, packlift_data_dir, LogHandler},
    APP_ID,
};
use serde_json::Value;

fn show_host_access_dialog(parent: &adw::ApplicationWindow) {
    let command = format!("flatpak override --user --talk-name=org.freedesktop.Flatpak {APP_ID}");
    let dialog = gtk::Dialog::builder()
        .title(tr("Allow access to your system"))
        .transient_for(parent)
        .modal(true)
        .default_width(460)
        .build();
    dialog.add_button(tr("Close"), gtk::ResponseType::Close);
    let copy = dialog.add_button(tr("Copy command"), gtk::ResponseType::Apply);
    dialog.set_default_response(gtk::ResponseType::Apply);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let explanation = gtk::Label::new(Some(
        tr("Packlift needs to run commands on your Linux system to use Toolbx, install packages and open programs. This permission allows commands to run with your account's access. Copy and run the command in your terminal, then close all Packlift windows and reopen the app."),
    ));
    explanation.set_wrap(true);
    explanation.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    explanation.set_xalign(0.0);
    content.append(&explanation);
    let command_label = gtk::Label::new(Some(&command));
    command_label.add_css_class("monospace");
    command_label.set_wrap(true);
    command_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    command_label.set_xalign(0.0);
    command_label.set_selectable(true);
    content.append(&command_label);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Apply {
            dialog.clipboard().set_text(&command);
            if let Some(button) = copy.downcast_ref::<gtk::Button>() {
                button.set_label(tr("Command copied"));
                button.announce(
                    tr("Command copied"),
                    gtk::AccessibleAnnouncementPriority::Medium,
                );
            }
        } else {
            dialog.close();
        }
    });
    dialog.present();
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|_| {
        if let Some(display) = gtk::gdk::Display::default() {
            let css = gtk::CssProvider::new();
            css.load_from_data(
                ".packlift-home-hero { padding: 20px; border-radius: 16px; background-color: alpha(@accent_bg_color, 0.12); border: 1px solid alpha(@accent_bg_color, 0.25); }
                 .packlift-home-icon { color: @accent_color; padding: 10px; border-radius: 12px; background-color: alpha(@accent_bg_color, 0.15); }
                 .packlift-package-hero { padding: 16px; border-radius: 14px; background-image: linear-gradient(125deg, alpha(@accent_bg_color, 0.17), alpha(@accent_bg_color, 0.04)); border: 1px solid alpha(@accent_bg_color, 0.22); }
                 .packlift-package-icon { padding: 9px; border-radius: 12px; background-color: alpha(@card_bg_color, 0.8); }
                 .packlift-package-note { padding: 10px 12px; border-radius: 9px; background-color: @card_bg_color; }",
            );
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(build_home);
    app.connect_open(|app, files, _hint| {
        for file in files {
            if let Some(path) = file.path() {
                open_package(app, &path);
            }
        }
    });
    app.run()
}

fn installer() -> Result<Installer> {
    let logger: LogHandler = Arc::new(|_| {});
    Installer::new(logger, None, None)
}

fn installer_with_output(logger: LogHandler) -> Result<Installer> {
    Installer::new(logger, None, None)
}

fn build_home(app: &adw::Application) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Packlift"));
    window.set_default_size(720, 540);
    render_home(app, &window);
    window.present();
}

fn render_home(app: &adw::Application, window: &adw::ApplicationWindow) {
    window.set_widget_name("packlift-library");
    window.set_resizable(true);
    let host_access = host_access_available();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Packlift", "")));

    let (menu, popover, actions) = action_menu(tr("Packlift menu"), "open-menu-symbolic");
    let parent = window.downgrade();
    add_menu_item(&popover, &actions, tr("Settings"), move || {
        if let Some(window) = parent.upgrade() {
            settings_dialog(&window);
        }
    });
    let parent = window.downgrade();
    add_menu_item(&popover, &actions, tr("View Packlift logs"), move || {
        if let Some(window) = parent.upgrade() {
            show_app_logs(&window);
        }
    });
    if host_access {
        let parent = window.downgrade();
        let app = app.clone();
        add_menu_item(
            &popover,
            &actions,
            tr("Clean unused environments…"),
            move || {
                let Some(window) = parent.upgrade() else {
                    return;
                };
                let app = app.clone();
                confirm_library_action(
                    &window,
                    tr("Clean unused environments?"),
                    tr("Packlift will remove containers it created that no installed program uses."),
                    tr("Clean"),
                    false,
                    move |window| {
                        run_job(&app, window, tr("Cleaning unused environments"), |logger| {
                            installer_with_output(logger)?
                                .clean_unused_environments()
                                .map(|_| None)
                        })
                    },
                );
            },
        );
    }
    header.pack_end(&menu);
    toolbar.add_top_bar(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(22);
    content.set_margin_bottom(24);
    content.set_margin_start(22);
    content.set_margin_end(22);

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    hero.add_css_class("packlift-home-hero");
    let package_icon = gtk::Image::from_icon_name("package-x-generic-symbolic");
    package_icon.set_pixel_size(38);
    package_icon.set_valign(gtk::Align::Start);
    package_icon.add_css_class("packlift-home-icon");
    hero.append(&package_icon);

    let intro = gtk::Box::new(gtk::Orientation::Vertical, 8);
    intro.set_hexpand(true);
    let title = home_label(tr("Add a program or game"), "title-2");
    intro.append(&title);
    intro.append(&home_label(
        if host_access {
            tr("Choose a Linux package, AppImage, Windows installer or game executable.")
        } else {
            tr("Add a compatible type 2 AppImage or Linux TAR archive to this library.")
        },
        "dim-label",
    ));
    let open = text_button(tr("Choose package…"));
    open.add_css_class("suggested-action");
    open.set_halign(gtk::Align::Start);
    open.set_margin_top(4);
    let parent = window.downgrade();
    let application = app.clone();
    open.connect_clicked(move |_| {
        let Some(window) = parent.upgrade() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(tr("Choose a package"))
            .build();
        let app = application.clone();
        dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    open_package(&app, &path);
                }
            }
        });
    });
    intro.append(&open);
    hero.append(&intro);
    content.append(&hero);

    if !host_access && !cfg!(feature = "sandbox-only") {
        let access = adw::ActionRow::builder()
            .title(tr("Sandbox mode"))
            .subtitle(tr(
                "Enable system access to use Flatpak, Toolbx, Wine or Proton.",
            ))
            .build();
        access.add_css_class("card");
        access.add_prefix(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
        let enable = text_button(tr("System access…"));
        enable.set_valign(gtk::Align::Center);
        let parent = window.downgrade();
        enable.connect_clicked(move |_| {
            if let Some(window) = parent.upgrade() {
                show_host_access_dialog(&window);
            }
        });
        access.add_suffix(&enable);
        let card = gtk::ListBox::new();
        card.set_selection_mode(gtk::SelectionMode::None);
        card.add_css_class("boxed-list");
        card.append(&access);
        content.append(&card);
    }

    let library = gtk::Box::new(gtk::Orientation::Vertical, 12);
    library.append(&home_label(tr("Your library"), "title-2"));
    match installer().and_then(|manager| manager.registry.apps()) {
        Ok(records) if records.is_empty() => library.append(&library_status(
            "folder-symbolic",
            tr("Your library is empty"),
            tr("Choose a package above to add your first program or game."),
        )),
        Ok(mut records) => {
            records
                .sort_by_cached_key(|record| record["name"].as_str().unwrap_or("").to_lowercase());
            let search = gtk::SearchEntry::new();
            search.set_placeholder_text(Some(tr("Search programs and games")));
            search.update_property(&[gtk::accessible::Property::Label(tr(
                "Search programs and games",
            ))]);
            library.append(&search);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("boxed-list");
            let rows: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let row = library_row(app, window, &record);
                    let searchable =
                        format!("{} {}", row.title(), row.subtitle().unwrap_or_default())
                            .to_lowercase();
                    list.append(&row);
                    (row, searchable)
                })
                .collect();
            library.append(&list);
            let empty = library_status(
                "system-search-symbolic",
                tr("No matches"),
                tr("Try a different name or runtime."),
            );
            empty.set_visible(false);
            library.append(&empty);
            search.connect_search_changed(move |entry| {
                let query = entry.text().trim().to_lowercase();
                let mut matches = false;
                for (row, searchable) in &rows {
                    let visible = searchable.contains(&query);
                    row.set_visible(visible);
                    matches |= visible;
                }
                list.set_visible(matches);
                empty.set_visible(!matches);
            });
        }
        Err(error) => library.append(&library_status(
            "dialog-error-symbolic",
            tr("Could not load your library"),
            &error.to_string(),
        )),
    }
    content.append(&library);

    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(560)
        .child(&content)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    toolbar.set_content(Some(&scroll));
    window.set_content(Some(&toolbar));
}

fn home_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.add_css_class(class);
    label
}

fn text_button(text: &str) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    if let Some(label) = button.child().and_downcast::<gtk::Label>() {
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_justify(gtk::Justification::Center);
        label.set_max_width_chars(32);
    }
    button
}

fn library_status(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let status = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .build();
    status.add_css_class("compact");
    status.add_css_class("card");
    status
}

fn action_menu(label: &str, icon: &str) -> (gtk::MenuButton, gtk::Popover, gtk::Box) {
    let menu = gtk::MenuButton::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .build();
    menu.update_property(&[gtk::accessible::Property::Label(label)]);
    menu.add_css_class("flat");
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let popover = gtk::Popover::builder().child(&actions).build();
    menu.set_popover(Some(&popover));
    (menu, popover, actions)
}

fn add_menu_item(
    popover: &gtk::Popover,
    actions: &gtk::Box,
    label: &str,
    action: impl Fn() + 'static,
) {
    let item = text_button(label);
    item.add_css_class("flat");
    item.set_halign(gtk::Align::Fill);
    let popover = popover.downgrade();
    item.connect_clicked(move |_| {
        if let Some(popover) = popover.upgrade() {
            popover.popdown();
        }
        action();
    });
    actions.append(&item);
}

fn library_row(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    record: &Value,
) -> adw::ActionRow {
    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let game = record["game_shortcut"].as_bool() == Some(true);
    let method = record["method"].as_str().unwrap_or("local");
    let subtitle = match method {
        "wine" => {
            let kind = if game {
                tr("Game shortcut")
            } else {
                tr("Windows program")
            };
            format!("{kind} · {}", program_runtime_label(record))
        }
        "toolbox" => format!(
            "Toolbx · {}",
            record["container"]
                .as_str()
                .unwrap_or(tr("Container"))
                .trim_start_matches("flatapk-")
        ),
        "native" => tr("Installed on this system").into(),
        "flatpak" => tr("Flatpak for your account").into(),
        _ if record["sandbox_only"].as_bool() == Some(true) => tr("Sandbox application").into(),
        _ => tr("Local Linux application").into(),
    };
    let subtitle = match record["version"]
        .as_str()
        .filter(|version| !version.is_empty())
    {
        Some(version) => format!("{subtitle} · {version}"),
        None => subtitle,
    };
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(name)
        .subtitle(&subtitle)
        .title_lines(1)
        .subtitle_lines(2)
        .build();
    row.add_prefix(&record_image(record, 42));

    if record["exports"][0]["argv"].is_array() {
        let launch = gtk::Button::with_label(tr("Open"));
        launch.set_valign(gtk::Align::Center);
        launch.update_property(&[gtk::accessible::Property::Label(
            &tr("Open {name}").replace("{name}", name),
        )]);
        let record = record.clone();
        let parent = window.downgrade();
        launch.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (sender, receiver) = std::sync::mpsc::channel();
            let record = record.clone();
            thread::spawn(move || {
                let _ = sender.send(installer().and_then(|manager| manager.launch(&record)));
            });
            let button = button.downgrade();
            let parent = parent.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return glib::ControlFlow::Continue
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(
                        PackliftError::Message(tr("Could not start the program.").into()),
                    ),
                };
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(true);
                }
                if let (Err(error), Some(window)) = (result, parent.upgrade()) {
                    show_error(&window, &error.to_string());
                }
                glib::ControlFlow::Break
            });
        });
        row.add_suffix(&launch);
    }

    let (menu, popover, actions) = action_menu(
        &tr("More options for {name}").replace("{name}", name),
        "view-more-symbolic",
    );
    menu.set_valign(gtk::Align::Center);
    if method == "wine" {
        let runtime = record["runtime"].as_str().unwrap_or_default().to_owned();
        let parent = window.downgrade();
        add_menu_item(&popover, &actions, tr("Open prefix folder"), move || {
            if let Some(window) = parent.upgrade() {
                open_prefix_folder(&window, &runtime);
            }
        });
        let app = app.clone();
        let record = record.clone();
        let parent = window.downgrade();
        add_menu_item(&popover, &actions, tr("Change runtime…"), move || {
            if let Some(window) = parent.upgrade() {
                program_runtime_dialog(&app, &window, &record);
            }
        });
    }
    if method == "toolbox" {
        let app = app.clone();
        let record = record.clone();
        let parent = window.downgrade();
        add_menu_item(
            &popover,
            &actions,
            tr("Repair installation…"),
            move || {
                let Some(window) = parent.upgrade() else {
                    return;
                };
                let app = app.clone();
                let record = record.clone();
                confirm_library_action(
                &window, tr("Repair this installation?"),
                tr("Packlift will reinstall the package in Toolbx, check dependencies and recreate its shortcuts."),
                tr("Repair"), false, move |window| {
                    let record = record.clone();
                    run_job(&app, window, tr("Repairing installation"), move |logger| {
                        installer_with_output(logger)?.repair_container_app(&record).map(Some)
                    });
                },
            );
            },
        );
    }

    let (remove, detail) = match (game, method) {
        (true, _) => (
            tr("Remove shortcut"),
            tr("The game files will stay in their original folder."),
        ),
        (_, "wine") => (
            tr("Remove from Packlift"),
            tr("This removes the program's library entry, shortcut and managed installation files."),
        ),
        _ => (
            tr("Uninstall"),
            tr("This removes the installed program and its shortcuts."),
        ),
    };
    let name = name.to_owned();
    let record = record.clone();
    let app = app.clone();
    let parent = window.downgrade();
    add_menu_item(&popover, &actions, &format!("{remove}…"), move || {
        let Some(window) = parent.upgrade() else {
            return;
        };
        let record = record.clone();
        let app = app.clone();
        confirm_library_action(
            &window,
            &format!("{remove}: {name}?"),
            detail,
            remove,
            true,
            move |window| {
                let record = record.clone();
                run_job(
                    &app,
                    window,
                    tr("Removing from your library"),
                    move |logger| {
                        installer_with_output(logger)?
                            .uninstall(&record)
                            .map(|_| None)
                    },
                );
            },
        );
    });
    row.add_suffix(&menu);
    row
}

fn confirm_library_action(
    window: &adw::ApplicationWindow,
    title: &str,
    description: &str,
    label: &str,
    destructive: bool,
    action: impl Fn(&adw::ApplicationWindow) + 'static,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .text(title)
        .secondary_text(description)
        .build();
    dialog.add_button(tr("Cancel"), gtk::ResponseType::Cancel);
    let accept = dialog.add_button(label, gtk::ResponseType::Accept);
    if destructive {
        accept.add_css_class("destructive-action");
    }
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let parent = window.downgrade();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Accept {
            if let Some(window) = parent.upgrade() {
                action(&window);
            }
        }
    });
    dialog.present();
}

fn program_runtime_label(record: &Value) -> String {
    if record["runtime"] != "proton" {
        return "Wine".into();
    }
    record["proton_path"]
        .as_str()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| tr("Proton (automatic)").into())
}

fn program_runtime_dialog(app: &adw::Application, window: &adw::ApplicationWindow, record: &Value) {
    let Some(id) = record["id"].as_str().map(str::to_owned) else {
        return;
    };
    let manager = match installer() {
        Ok(manager) => manager,
        Err(error) => {
            show_error(window, &error.to_string());
            return;
        }
    };
    let versions = manager.installed_proton_versions();
    let saved = record["proton_path"].as_str().map(Path::new);
    let mut names = vec!["Wine".to_owned(), tr("Proton (automatic)").to_owned()];
    names.extend(versions.iter().map(|path| {
        format!(
            "Proton · {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    }));
    let labels = names.iter().map(String::as_str).collect::<Vec<_>>();
    let choices = gtk::DropDown::from_strings(&labels);
    choices.update_property(&[gtk::accessible::Property::Label(tr(
        "Runtime for this program",
    ))]);
    let selected = if record["runtime"] == "proton" {
        saved
            .and_then(|path| versions.iter().position(|version| version == path))
            .map(|index| index + 2)
            .unwrap_or(1)
    } else {
        0
    };
    choices.set_selected(selected as u32);

    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let dialog = gtk::Dialog::builder()
        .title(tr("Runtime for {name}").replace("{name}", name))
        .transient_for(window)
        .modal(true)
        .default_width(420)
        .build();
    dialog.add_button(tr("Cancel"), gtk::ResponseType::Cancel);
    dialog
        .add_button(tr("Apply"), gtk::ResponseType::Accept)
        .add_css_class("suggested-action");
    let body = dialog.content_area();
    body.set_spacing(12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.append(&home_label(tr("Windows runtime"), "heading"));
    body.append(&choices);
    body.append(&home_label(
        if versions.is_empty() {
            tr("Proton (automatic) uses UMU to download Proton as needed. Installed Proton versions will appear here.")
        } else {
            tr("Proton (automatic) uses UMU to manage the version. Pick an installed version to use it for this app.")
        },
        "dim-label",
    ));
    body.append(&home_label(
        tr("This setting applies to this program and its system menu shortcut."),
        "dim-label",
    ));
    if saved.is_some() && selected == 1 {
        body.append(&home_label(
            tr("The previously selected Proton version is no longer installed."),
            "warning",
        ));
    }
    let app = app.clone();
    let parent = window.downgrade();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response != gtk::ResponseType::Accept {
            return;
        }
        let Some(window) = parent.upgrade() else {
            return;
        };
        let selected = choices.selected();
        let selected_proton = selected
            .checked_sub(2)
            .and_then(|i| versions.get(i as usize))
            .cloned();
        let runtime = if selected == 0 { "wine" } else { "proton" };
        let id = id.clone();
        run_job(&app, &window, tr("Saving Windows runtime"), move |logger| {
            installer_with_output(logger)?
                .set_program_runtime(&id, runtime, selected_proton.as_deref())
                .map(|_| None)
        });
    });
    dialog.present();
}

fn open_package(app: &adw::Application, path: &Path) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some(tr("Review package")));
    window.set_default_size(470, -1);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Review package"), "")));
    toolbar.add_top_bar(&header);
    toolbar.set_bottom_bar_style(adw::ToolbarStyle::RaisedBorder);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);

    match installer().and_then(|manager| manager.probe(path)) {
        Ok((package, plan)) => {
            body.append(&package_identity(&package, path));

            let options = gtk::ListBox::new();
            options.set_selection_mode(gtk::SelectionMode::None);
            options.add_css_class("boxed-list");
            let ambiguous_rpm = package.kind == "rpm" && package.rpm_family.is_empty();
            let destination = match plan.method.as_str() {
                "wine" => tr("Windows compatibility environment").to_owned(),
                "native" => tr("This Linux system").to_owned(),
                "flatpak" => tr("Flatpak for your account").to_owned(),
                "toolbox" if ambiguous_rpm => tr("A Toolbx container").to_owned(),
                "toolbox" => format!("Toolbx · {}", plan.family),
                _ if !host_access_available() => tr("Packlift sandbox").to_owned(),
                _ => tr("Packlift library").to_owned(),
            };
            let target = adw::ActionRow::builder()
                .title(tr("Installation destination"))
                .use_markup(false)
                .subtitle(&destination)
                .build();
            target.add_prefix(&gtk::Image::from_icon_name("drive-harddisk-symbolic"));
            options.append(&target);

            let source = adw::ExpanderRow::builder()
                .title(tr("Package file"))
                .use_markup(false)
                .subtitle_lines(1)
                .subtitle(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .as_ref(),
                )
                .build();
            source.add_prefix(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            let full_path = home_label(&path.to_string_lossy(), "dim-label");
            full_path.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            full_path.set_selectable(true);
            full_path.set_margin_start(14);
            full_path.set_margin_end(14);
            full_path.set_margin_top(10);
            full_path.set_margin_bottom(10);
            source.add_row(&full_path);
            options.append(&source);
            if package.kind == "flatpakref" && !package.description.is_empty() {
                let repository = adw::ActionRow::builder()
                    .title(tr("Flatpak repository"))
                    .use_markup(false)
                    .subtitle_lines(2)
                    .subtitle(&package.description)
                    .build();
                options.append(&repository);
            }
            body.append(&options);

            let runtime = gtk::DropDown::from_strings(&["Wine", "Proton"]);
            if ["exe", "setup", "msi"].contains(&package.kind.as_str()) {
                if installer()
                    .map(|m| m.preferences().0 == "proton")
                    .unwrap_or(false)
                {
                    runtime.set_selected(1);
                }
                let choice = adw::ActionRow::builder()
                    .title(tr("Windows runtime"))
                    .build();
                runtime.update_property(&[gtk::accessible::Property::Label(tr("Windows runtime"))]);
                runtime.set_valign(gtk::Align::Center);
                choice.add_suffix(&runtime);
                choice.set_activatable_widget(Some(&runtime));
                options.insert(&choice, 1);
            }

            let libraries = adw::SwitchRow::builder()
                .title(tr("Install missing libraries"))
                .subtitle(tr("Search the container repositories for dependencies."))
                .build();
            if plan.method == "toolbox" {
                libraries.set_active(true);
                options.insert(&libraries, 1);
            }
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            actions.set_halign(gtk::Align::End);
            actions.set_margin_start(16);
            actions.set_margin_end(16);
            actions.set_margin_top(12);
            actions.set_margin_bottom(12);
            let cancel = text_button(tr("Cancel"));
            let weak_window = window.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
            });
            actions.append(&cancel);
            let rpm_target = gtk::DropDown::from_strings(&["Fedora", "openSUSE"]);
            let choose_target = ambiguous_rpm;
            if choose_target {
                let distribution = adw::ActionRow::builder()
                    .title(tr("RPM distribution"))
                    .build();
                rpm_target
                    .update_property(&[gtk::accessible::Property::Label(tr("RPM distribution"))]);
                rpm_target.set_valign(gtk::Align::Center);
                distribution.add_suffix(&rpm_target);
                distribution.set_activatable_widget(Some(&rpm_target));
                options.insert(&distribution, 1);
            }
            let verified = package.signature == "verified";
            body.append(&package_note(
                if verified {
                    "emblem-ok-symbolic"
                } else {
                    "dialog-warning-symbolic"
                },
                if verified {
                    tr("Signature verified")
                } else {
                    tr("Unverified signature")
                },
                if verified {
                    tr("Signed with a trusted key.")
                } else {
                    tr("Install only if you trust the source of this file.")
                },
                if verified { "success" } else { "warning" },
            ));
            if plan.method == "wine" {
                body.append(&package_note(
                    "dialog-information-symbolic",
                    tr("Home folder access"),
                    tr("Windows programs can access files in your home folder."),
                    "dim-label",
                ));
            }
            let setup = if plan.method == "toolbox" {
                installer().and_then(|m| m.container_setup())
            } else {
                Ok(None)
            };
            let install = text_button(if matches!(&setup, Ok(Some(_))) {
                tr("Set up and install")
            } else if package.kind == "exe" {
                tr("Continue")
            } else {
                tr("Install")
            });
            match &setup {
                Ok(Some(args)) => {
                    let mut summary = tr("Authorize this host setup command: {command}")
                        .replace("{command}", &args.join(" "));
                    if matches!(args[0].as_str(), "rpm-ostree" | "transactional-update") {
                        summary.push(' ');
                        summary.push_str(tr("Restart required."));
                    }
                    body.append(&package_note(
                        "dialog-information-symbolic",
                        tr("Toolbx setup"),
                        &summary,
                        "dim-label",
                    ));
                }
                Err(error) => {
                    body.append(&notice_row(
                        "dialog-warning-symbolic",
                        &error.to_string(),
                        "warning",
                    ));
                    install.set_sensitive(false);
                }
                _ => {}
            }
            let setup = setup.ok().flatten();
            install.add_css_class("suggested-action");
            let app = app.clone();
            let package = package.clone();
            let kind = package.kind.clone();
            let selected_runtime = runtime.clone();
            let weak_window = window.downgrade();
            install.connect_clicked(move |_| {
                let runtime = if selected_runtime.selected() == 1 {
                    "proton"
                } else {
                    "wine"
                }
                .to_owned();
                if kind == "exe" {
                    if let Some(window) = weak_window.upgrade() {
                        choose_windows_action(&app, &window, &package, &runtime);
                    }
                } else if let Some(window) = weak_window.upgrade() {
                    let setup = setup.clone();
                    let mut package = package.clone();
                    package.resolve_libraries = libraries.is_active();
                    if choose_target {
                        package.use_container = true;
                        package.rpm_family = if rpm_target.selected() == 1 {
                            "opensuse"
                        } else {
                            "fedora"
                        }
                        .into();
                    }
                    let runtime = runtime.clone();
                    if kind == "setup" {
                        run_job(
                            &app,
                            &window,
                            tr("Installing program or game"),
                            move |logger| {
                                installer_with_output(logger)?
                                    .install_windows(&package, &runtime)
                                    .map(Some)
                            },
                        );
                    } else {
                        run_job(&app, &window, tr("Installing program"), move |logger| {
                            let manager = installer_with_output(logger)?;
                            if let Some(command) = setup {
                                manager.setup_container_support(&command)?;
                            }
                            manager.install_package(&package, &runtime).map(Some)
                        });
                    }
                }
            });
            actions.append(&install);
            toolbar.add_bottom_bar(&actions);
        }
        Err(error) => {
            if matches!(error, PackliftError::HostAccessRequired) && !cfg!(feature = "sandbox-only")
            {
                let enable = text_button(tr("Enable access to your system…"));
                enable.set_halign(gtk::Align::Start);
                let parent = window.downgrade();
                enable.connect_clicked(move |_| {
                    if let Some(window) = parent.upgrade() {
                        show_host_access_dialog(&window);
                    }
                });
                body.append(&enable);
            }
            let title = gtk::Label::new(Some(tr("Could not open package")));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            body.append(&title);
            let message = if matches!(error, PackliftError::HostAccessRequired) {
                tr("This format needs system access. In sandbox mode, choose a type 2 AppImage or a Linux TAR archive.").into()
            } else {
                error.to_string()
            };
            body.append(&notice_row("dialog-error-symbolic", &message, "error"));
            let file = gtk::Label::new(Some(&path.display().to_string()));
            file.set_wrap(true);
            file.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            file.set_xalign(0.0);
            file.add_css_class("dim-label");
            body.append(&file);
        }
    }
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    scroll.set_max_content_height(480);
    scroll.set_child(Some(&body));
    toolbar.set_content(Some(&scroll));
    window.set_content(Some(&toolbar));
    window.present();
}

fn package_identity(package: &Package, path: &Path) -> gtk::Box {
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    hero.add_css_class("packlift-package-hero");
    let icon = package_image(package, 54);
    icon.set_valign(gtk::Align::Center);
    icon.add_css_class("packlift-package-icon");
    hero.append(&icon);

    let identity = gtk::Box::new(gtk::Orientation::Vertical, 5);
    identity.set_valign(gtk::Align::Center);
    identity.set_hexpand(true);
    let title = home_label(&package.name, "title-2");
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    identity.append(&title);

    let mut metadata = vec![package.kind.to_uppercase()];
    if !package.architecture.is_empty() {
        metadata.push(package.architecture.clone());
    }
    if let Ok(file) = std::fs::metadata(path) {
        metadata.push(human_size(file.len()));
    }
    identity.append(&home_label(&metadata.join(" · "), "dim-label"));
    if !package.version.is_empty() {
        identity.append(&home_label(
            &tr("Version {version}").replace("{version}", &package.version),
            "dim-label",
        ));
    }
    hero.append(&identity);
    hero
}

fn package_note(icon_name: &str, title: &str, detail: &str, style: &str) -> gtk::Box {
    let note = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    note.add_css_class("packlift-package-note");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(18);
    icon.set_valign(gtk::Align::Start);
    icon.add_css_class(style);
    note.append(&icon);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
    labels.set_hexpand(true);
    labels.append(&home_label(title, "heading"));
    let detail = home_label(detail, "dim-label");
    detail.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    labels.append(&detail);
    note.append(&labels);
    note
}

fn package_image(package: &Package, size: i32) -> gtk::Image {
    let texture = package
        .icon
        .as_ref()
        .filter(|path| path.is_file())
        .and_then(|path| gtk::gdk::Texture::from_file(&gio::File::for_path(path)).ok());
    let image = texture
        .as_ref()
        .map(|texture| gtk::Image::from_paintable(Some(texture)))
        .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-executable"));
    image.set_pixel_size(size);
    image
}

fn notice_row(icon_name: &str, text: &str, style: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("card");
    row.set_margin_top(2);
    row.set_margin_bottom(2);
    row.set_margin_start(2);
    row.set_margin_end(2);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(18);
    icon.add_css_class(style);
    icon.set_valign(gtk::Align::Start);
    row.append(&icon);
    let message = gtk::Label::new(Some(text));
    message.set_wrap(true);
    message.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    message.set_xalign(0.0);
    message.set_hexpand(true);
    message.add_css_class(style);
    row.append(&message);
    row
}

fn choose_windows_action(
    app: &adw::Application,
    parent: &adw::ApplicationWindow,
    package: &Package,
    runtime: &str,
) {
    let dialog = gtk::Window::builder()
        .title(tr("Choose how to use this .exe"))
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .default_width(460)
        .default_height(390)
        .build();

    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.set_margin_top(24);
    body.set_margin_bottom(22);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let image = package_image(package, 56);
    let icon_frame = gtk::Frame::new(None);
    icon_frame.add_css_class("card");
    icon_frame.set_child(Some(&image));
    hero.append(&icon_frame);
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 5);
    let title = gtk::Label::new(Some(tr("What is this .exe file?")));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    heading.append(&title);
    let subtitle = gtk::Label::new(Some(tr(
        "Choose the option that matches the file you selected.",
    )));
    subtitle.add_css_class("dim-label");
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);
    heading.append(&subtitle);
    hero.append(&heading);
    body.append(&hero);

    let package_to_add = package.clone();
    let runtime_to_add = runtime.to_owned();
    let app_to_add = app.clone();
    let parent_to_add = parent.clone();
    let dialog_to_add = dialog.clone();
    let add = choice_button(
        tr("Add an installed program or game"),
        tr("Creates a menu shortcut for this .exe. Use this if the game or app is already installed."),
        false,
    );
    add.connect_clicked(move |_| {
        dialog_to_add.close();
        let package = package_to_add.clone();
        let runtime = runtime_to_add.clone();
        run_job(
            &app_to_add,
            &parent_to_add,
            tr("Adding program or game to menu"),
            move |logger| {
                installer_with_output(logger)?
                    .add_game(&package, &runtime)
                    .map(Some)
            },
        );
    });
    body.append(&add);

    let package_to_install = package.clone();
    let runtime_to_install = runtime.to_owned();
    let app_to_install = app.clone();
    let parent_to_install = parent.clone();
    let dialog_to_install = dialog.clone();
    let install = choice_button(
        tr("Run this as an installer"),
        tr("Use this if the .exe contains setup and needs to install the program first."),
        true,
    );
    install.connect_clicked(move |_| {
        dialog_to_install.close();
        let package = package_to_install.clone();
        let runtime = runtime_to_install.clone();
        run_job(
            &app_to_install,
            &parent_to_install,
            tr("Installing program or game"),
            move |logger| {
                installer_with_output(logger)?
                    .install_windows(&package, &runtime)
                    .map(Some)
            },
        );
    });
    body.append(&install);

    let cancel = text_button(tr("Cancel"));
    cancel.set_hexpand(true);
    let dialog_to_close = dialog.clone();
    cancel.connect_clicked(move |_| dialog_to_close.close());
    body.append(&cancel);
    dialog.set_child(Some(&body));
    dialog.present();
}

fn choice_button(title: &str, description: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::new();
    if suggested {
        button.add_css_class("suggested-action");
    } else {
        button.add_css_class("card");
    }
    button.set_hexpand(true);
    button.set_size_request(-1, 82);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let title_label = gtk::Label::new(Some(title));
    title_label.add_css_class("heading");
    title_label.set_xalign(0.0);
    let description_label = gtk::Label::new(Some(description));
    description_label.add_css_class("dim-label");
    description_label.set_wrap(true);
    description_label.set_xalign(0.0);
    content.append(&title_label);
    content.append(&description_label);
    button.set_child(Some(&content));
    button
}

fn run_job<F>(app: &adw::Application, window: &adw::ApplicationWindow, title: &str, task: F)
where
    F: FnOnce(LogHandler) -> Result<Option<Value>> + Send + 'static,
{
    let from_library = window.widget_name() == "packlift-library";
    if !from_library {
        window.set_default_size(380, 220);
    }
    window.set_resizable(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(14);
    body.set_margin_bottom(12);
    body.set_margin_start(20);
    body.set_margin_end(20);
    let heading = gtk::Label::new(Some(title));
    heading.set_wrap(true);
    heading.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    heading.add_css_class("title-2");
    body.append(&heading);
    let stage = gtk::Label::new(Some(tr("Preparing the installation environment…")));
    stage.set_wrap(true);
    stage.set_xalign(0.0);
    stage.add_css_class("dim-label");
    body.append(&stage);
    let progress = gtk::ProgressBar::new();
    progress.update_property(&[gtk::accessible::Property::Label(tr(
        "Installation progress",
    ))]);
    progress.set_show_text(false);
    progress.set_fraction(0.08);
    body.append(&progress);
    let spinner = gtk::Spinner::new();
    spinner.start();
    body.append(&spinner);
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let buffer = gtk::TextBuffer::new(None);
    let text = gtk::TextView::with_buffer(&buffer);
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    let log_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(130)
        .child(&text)
        .build();
    let copy = text_button(tr("Copy log"));
    copy.set_halign(gtk::Align::End);
    let copy_log = log.clone();
    copy.connect_clicked(move |button| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&copy_log.borrow().join("\n"));
            button.set_label(tr("Log copied"));
        }
    });
    let details = gtk::Expander::builder().label(tr("Show details")).build();
    let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    detail_box.append(&log_scroll);
    detail_box.append(&copy);
    details.set_child(Some(&detail_box));
    body.append(&details);
    window.set_content(Some(&body));

    enum JobEvent {
        Log(String),
        Finished(Result<Option<Value>>),
    }
    let (sender, receiver) = std::sync::mpsc::channel::<JobEvent>();
    thread::spawn(move || {
        let log_sender = sender.clone();
        let logger: LogHandler = Arc::new(move |line| {
            let _ = log_sender.send(JobEvent::Log(line.to_owned()));
        });
        let _ = sender.send(JobEvent::Finished(task(logger)));
    });
    let app = app.clone();
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(120), move || loop {
        match receiver.try_recv() {
            Ok(JobEvent::Log(line)) => {
                let (label, fraction) = installation_stage(&line);
                if label != tr("Working…") && stage.text().as_str() != label {
                    stage.set_text(label);
                    stage.announce(label, gtk::AccessibleAnnouncementPriority::Low);
                }
                progress.set_fraction(fraction);
                log.borrow_mut().push(line);
                buffer.set_text(&log.borrow().join("\n"));
            }
            Ok(JobEvent::Finished(result)) => {
                progress.set_fraction(if result.is_ok() { 1.0 } else { 0.0 });
                if let Some(window) = weak_window.upgrade() {
                    match result {
                        Err(PackliftError::InstalledExecutableRequired { package, runtime }) => {
                            choose_installed_executable(&app, &window, *package, runtime);
                        }
                        Err(PackliftError::ExecutableSelectionFailed {
                            package,
                            runtime,
                            reason,
                        }) => {
                            choose_installed_executable(&app, &window, *package, runtime);
                            show_error(&window, &reason);
                        }
                        Err(error) => {
                            if from_library {
                                render_home(&app, &window);
                            }
                            show_error(&window, &error.to_string());
                        }
                        Ok(Some(record)) => {
                            for other in app
                                .windows()
                                .into_iter()
                                .filter_map(|other| other.downcast::<adw::ApplicationWindow>().ok())
                            {
                                if other != window
                                    && other.widget_name() == "packlift-library"
                                    && other
                                        .content()
                                        .is_some_and(|content| content.is::<adw::ToolbarView>())
                                {
                                    render_home(&app, &other);
                                }
                            }
                            show_success(&window, &record);
                        }
                        Ok(None) => render_home(&app, &window),
                    }
                }
                return glib::ControlFlow::Break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
        }
    });
}

fn choose_installed_executable(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    package: Package,
    runtime: String,
) {
    window.set_default_size(440, -1);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        tr("Choose the installed executable"),
        "",
    )));
    toolbar.add_top_bar(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.append(&home_label(tr("Windows installer finished"), "title-2"));
    body.append(&home_label(tr("Packlift could not identify a single program to open. Select the game's .exe from the folder where it was installed."), "dim-label"));
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.append(&prefix_folder_button(window, &runtime));
    let choose = text_button(tr("Choose .exe…"));
    choose.add_css_class("suggested-action");
    actions.append(&choose);
    let parent = window.downgrade();
    let app = app.clone();
    choose.connect_clicked(move |_| {
        let Some(window) = parent.upgrade() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(tr("Choose the installed executable"))
            .build();
        if let Some(folder) = package.path.parent() {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let app = app.clone();
        let package = package.clone();
        let runtime = runtime.clone();
        let parent = window.downgrade();
        dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
            let (Ok(file), Some(window)) = (result, parent.upgrade()) else {
                return;
            };
            let Some(path) = file.path() else { return };
            run_job(
                &app,
                &window,
                tr("Adding program or game to menu"),
                move |logger| match installer_with_output(logger)?
                    .finish_windows_installation(&package, &runtime, &path)
                {
                    Ok(record) => Ok(Some(record)),
                    Err(error) => Err(PackliftError::ExecutableSelectionFailed {
                        package: Box::new(package),
                        runtime,
                        reason: error.to_string(),
                    }),
                },
            );
        });
    });
    body.append(&actions);
    toolbar.set_content(Some(&body));
    window.set_content(Some(&toolbar));
}

fn show_success(window: &adw::ApplicationWindow, record: &Value) {
    window.set_title(Some("Packlift"));
    window.set_default_size(430, 320);
    window.set_resizable(false);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(22);
    body.set_margin_bottom(22);
    body.set_margin_start(28);
    body.set_margin_end(28);
    body.set_valign(gtk::Align::Center);

    let icon = record_image(record, 72);
    let icon_frame = gtk::Frame::new(None);
    icon_frame.add_css_class("card");
    icon_frame.set_halign(gtk::Align::Center);
    icon_frame.set_child(Some(&icon));
    body.append(&icon_frame);

    let is_game = record["game_shortcut"].as_bool().unwrap_or(false);
    let title = gtk::Label::new(Some(if is_game {
        tr("Game added to Packlift")
    } else {
        tr("Installation complete")
    }));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Center);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body.append(&title);
    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let runnable = record["exports"][0]["argv"].is_array();
    let message = if runnable {
        tr("{name} is ready to use.").replace("{name}", name)
    } else if record["method"] == "flatpak" {
        tr("{name} was installed for your account.").replace("{name}", name)
    } else {
        tr("{name} is installed. This package has no application launcher.").replace("{name}", name)
    };
    let description = gtk::Label::new(Some(&message));
    description.set_wrap(true);
    description.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    description.set_halign(gtk::Align::Center);
    description.add_css_class("dim-label");
    body.append(&description);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(4);
    let close = text_button(tr("Close"));
    let weak_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    actions.append(&close);
    if record["method"].as_str() == Some("wine") {
        actions.append(&prefix_folder_button(
            window,
            record["runtime"].as_str().unwrap_or_default(),
        ));
    }
    let open = gtk::Button::with_label(tr("Open"));
    open.add_css_class("suggested-action");
    let record = record.clone();
    let weak_window = window.downgrade();
    open.connect_clicked(move |_| {
        if let Err(error) = installer().and_then(|manager| manager.launch(&record)) {
            if let Some(window) = weak_window.upgrade() {
                show_error(&window, &error.to_string());
            }
        } else if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    if runnable {
        actions.append(&open);
    }
    body.append(&actions);
    window.set_content(Some(&body));
}

fn prefix_folder_button(window: &adw::ApplicationWindow, runtime: &str) -> gtk::Button {
    let button = text_button(tr("Open prefix folder"));
    let parent = window.downgrade();
    let runtime = runtime.to_owned();
    button.connect_clicked(move |_| {
        if let Some(window) = parent.upgrade() {
            open_prefix_folder(&window, &runtime);
        }
    });
    button
}

fn open_prefix_folder(window: &adw::ApplicationWindow, runtime: &str) {
    let drive = windows_prefix_drive(&packlift_data_dir(), runtime);
    // The desktop portal opens the parent and selects the C: drive.
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(drive)));
    let parent = window.downgrade();
    launcher.open_containing_folder(Some(window), None::<&gio::Cancellable>, move |result| {
        if let (Err(error), Some(window)) = (result, parent.upgrade()) {
            let message = tr("Could not open the prefix folder: {error}")
                .replace("{error}", &error.to_string());
            show_error(&window, &message);
        }
    });
}

fn record_image(record: &Value, size: i32) -> gtk::Image {
    let icon = record["exports"][0]["icon"].as_str().unwrap_or("");
    let texture = if !icon.is_empty() && Path::new(icon).is_file() {
        gtk::gdk::Texture::from_file(&gio::File::for_path(icon)).ok()
    } else {
        None
    };
    let image = texture
        .as_ref()
        .map(|texture| gtk::Image::from_paintable(Some(texture)))
        .unwrap_or_else(|| {
            gtk::Image::from_icon_name(if !icon.is_empty() && !Path::new(icon).is_absolute() {
                icon
            } else {
                "application-x-executable"
            })
        });
    image.set_pixel_size(size);
    image
}

fn installation_stage(line: &str) -> (&'static str, f64) {
    let line = line.to_ascii_lowercase();
    if line.contains("analyzing") {
        (tr("Analyzing the package"), 0.12)
    } else if line.contains("compatibility") {
        (tr("Checking compatibility"), 0.25)
    } else if line.contains("environment") || line.contains("toolbox") || line.contains("prefix") {
        (tr("Preparing the installation environment"), 0.38)
    } else if line.contains("download")
        || line.contains("flatpak install")
        || line.contains("pulling")
    {
        (tr("Downloading dependencies"), 0.54)
    } else if line.contains("shortcut") || line.contains("menu") {
        (tr("Creating application shortcuts"), 0.90)
    } else if line.contains("install") || line.contains("package") {
        (tr("Installing the program"), 0.72)
    } else {
        (tr("Working…"), 0.64)
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn show_error(window: &adw::ApplicationWindow, message: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Close)
        .text(tr("Packlift could not complete the operation"))
        .secondary_text(message)
        .build();
    dialog.add_button(tr("Open log folder"), gtk::ResponseType::Help);
    dialog.add_button(tr("Copy log"), gtk::ResponseType::Apply);
    dialog.connect_response(|dialog, response| {
        if response == gtk::ResponseType::Help {
            let folder = gio::File::for_path(packlift_data_dir());
            let _ =
                gio::AppInfo::launch_default_for_uri(&folder.uri(), None::<&gio::AppLaunchContext>);
            return;
        }
        if response == gtk::ResponseType::Apply {
            let path = packlift_data_dir().join("flatapk.log");
            let contents = std::fs::read_to_string(path).unwrap_or_else(|error| {
                tr("Could not read the Packlift log: {error}")
                    .replace("{error}", &error.to_string())
            });
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&contents);
                dialog.set_secondary_text(Some(tr("Log copied to clipboard.")));
                return;
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn settings_dialog(window: &adw::ApplicationWindow) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some(tr("Packlift settings")));
    dialog.set_transient_for(Some(window));
    dialog.set_modal(true);
    dialog.add_button(tr("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(tr("Save"), gtk::ResponseType::Accept);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let choices = gtk::DropDown::from_strings(&["Wine", "Proton"]);
    choices.update_property(&[gtk::accessible::Property::Label(tr(
        "Default Windows runtime",
    ))]);
    if installer()
        .map(|m| m.preferences().0 == "proton")
        .unwrap_or(false)
    {
        choices.set_selected(1);
    }
    content.append(&gtk::Label::new(Some(tr("Default Windows runtime"))));
    content.append(&choices);
    let associations = text_button(tr("Set as default for supported files"));
    associations.set_sensitive(host_access_available());
    let association_status = gtk::Label::new(Some(
        tr("Applies to your account for DEB, RPM, Arch packages, AppImage, EXE, MSI, SETUP and supported TAR archives."),
    ));
    association_status.set_wrap(true);
    association_status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    association_status.set_xalign(0.0);
    association_status.add_css_class("dim-label");
    let status = association_status.clone();
    associations.connect_clicked(move |button| {
        button.set_sensitive(false);
        status.set_text(tr("Updating file associations…"));
        let (sender, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(installer().and_then(|manager| manager.set_default_file_handler()));
        });
        let button = button.downgrade();
        let status = status.downgrade();
        glib::timeout_add_local(Duration::from_millis(120), move || {
            let (Some(button), Some(status)) = (button.upgrade(), status.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            let message = match receiver.try_recv() {
                Ok(Ok(())) => tr("Packlift is now the default for supported files.").into(),
                Ok(Err(error)) => error.to_string(),
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    tr("Could not update file associations.").into()
                }
            };
            status.set_text(&message);
            status.announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
            button.set_sensitive(true);
            glib::ControlFlow::Break
        });
    });
    content.append(&associations);
    content.append(&association_status);
    let logs = text_button(tr("View Packlift logs"));
    let weak_window_for_logs = window.downgrade();
    logs.connect_clicked(move |_| {
        if let Some(parent) = weak_window_for_logs.upgrade() {
            show_app_logs(&parent);
        }
    });
    content.append(&logs);
    dialog.content_area().append(&content);
    let choices = choices.clone();
    let weak_window = window.downgrade();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Accept {
            let runtime = if choices.selected() == 1 {
                "proton"
            } else {
                "wine"
            }
            .to_owned();
            let weak_window = weak_window.clone();
            let task = move |logger| {
                installer_with_output(logger)
                    .and_then(|manager| manager.save_preferences(&runtime))
                    .map(|_| None)
            };
            if let Some(window) = weak_window.upgrade() {
                if let Some(app) = window.application().and_downcast::<adw::Application>() {
                    run_job(&app, &window, tr("Saving settings"), task);
                }
            }
        }
    });
    dialog.present();
}

fn show_app_logs(parent: &adw::ApplicationWindow) {
    let window = gtk::Window::builder()
        .title(tr("Packlift logs"))
        .transient_for(parent)
        .modal(true)
        .default_width(760)
        .default_height(520)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let log_path = packlift_data_dir().join("flatapk.log");
    let log = std::rc::Rc::new(gtk::TextView::new());
    log.set_editable(false);
    log.set_cursor_visible(false);
    log.set_monospace(true);
    log.set_wrap_mode(gtk::WrapMode::WordChar);
    log.buffer()
        .set_text(&std::fs::read_to_string(&log_path).unwrap_or_else(|error| {
            tr("No Packlift log is available yet. {error}").replace("{error}", &error.to_string())
        }));
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&*log)
        .build();
    body.append(&scroll);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let copy = text_button(tr("Copy all logs"));
    let log_path_copy = log_path.clone();
    copy.connect_clicked(move |button| {
        if let Ok(contents) = std::fs::read_to_string(&log_path_copy) {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&contents);
                button.set_label(tr("Logs copied"));
            }
        }
    });
    actions.append(&copy);
    let close = text_button(tr("Close"));
    let weak_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    actions.append(&close);
    body.append(&actions);
    window.set_child(Some(&body));
    window.present();
}
