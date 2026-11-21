use adw::prelude::*;
use gtk::prelude::*;
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc, thread, time::Duration};

use packlift::{
    core::{Package, PackliftError, Result},
    installer::Installer,
    system::LogHandler,
};
use serde_json::Value;

const APP_ID: &str = "io.github.flatapk.Installer";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE)
        .build();

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
    window.set_default_size(640, 470);
    render_home(app, &window);
    window.present();
}

fn render_home(app: &adw::Application, window: &adw::ApplicationWindow) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Packlift", "")));
    let settings = gtk::Button::from_icon_name("preferences-system-symbolic");
    settings.update_property(&[gtk::accessible::Property::Label("Packlift settings")]);
    settings.set_tooltip_text(Some("Packlift settings"));
    let weak_window = window.downgrade();
    settings.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            settings_dialog(&window);
        }
    });
    header.pack_end(&settings);
    toolbar.add_top_bar(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let open = gtk::Button::with_label("Open package");
    open.add_css_class("suggested-action");
    open.set_halign(gtk::Align::Start);
    let weak_window = window.downgrade();
    let application = app.clone();
    open.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            let dialog = gtk::FileDialog::builder().title("Open package").build();
            let app = application.clone();
            dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        open_package(&app, &path);
                    }
                }
            });
        }
    });
    content.append(&open);

    let programs = adw::PreferencesGroup::builder()
        .title("Installed programs and games")
        .description("Programs managed by Packlift")
        .build();
    match installer().and_then(|manager| manager.registry.apps()) {
        Ok(apps) => {
            for record in apps {
                let name = record["name"].as_str().unwrap_or("Program");
                let game = record["game_shortcut"].as_bool().unwrap_or(false);
                let method = record["method"].as_str().unwrap_or("local");
                let runtime = if record["runtime"].as_str().unwrap_or("") == "proton" {
                    "Proton"
                } else {
                    "Wine"
                };
                let version = record["version"].as_str().filter(|value| !value.is_empty());
                let row = adw::ActionRow::builder()
                    .title(name)
                    .subtitle(match (game, version) {
                        (true, Some(version)) => format!("Game shortcut · {runtime} · {version}"),
                        (true, None) => format!("Game shortcut · {runtime}"),
                        (false, Some(version)) => format!("{method} · {version}"),
                        (false, None) => method.to_owned(),
                    })
                    .build();
                if let Some(icon) = record["exports"][0]["icon"].as_str() {
                    let image = if Path::new(icon).is_absolute() {
                        gtk::Image::from_file(icon)
                    } else {
                        gtk::Image::from_icon_name(icon)
                    };
                    image.set_pixel_size(32);
                    row.add_prefix(&image);
                }
                let launch = gtk::Button::with_label("Open");
                launch.set_valign(gtk::Align::Center);
                let record_to_open = record.clone();
                let weak_window = window.downgrade();
                launch.connect_clicked(move |_| {
                    let result = installer().and_then(|manager| manager.launch(&record_to_open));
                    if let Err(error) = result {
                        if let Some(window) = weak_window.upgrade() {
                            show_error(&window, &error.to_string());
                        }
                    }
                });
                if record["exports"][0]["argv"].is_array() {
                    row.add_suffix(&launch);
                }
                if method == "toolbox" {
                    let repair = gtk::Button::with_label("Repair…");
                    let parent = window.downgrade();
                    let application = app.clone();
                    let record = record.clone();
                    repair.connect_clicked(move |_| {
                        let Some(window) = parent.upgrade() else { return };
                        let confirm = gtk::MessageDialog::builder().transient_for(&window).modal(true)
                            .text("Repair this container installation?")
                            .secondary_text("Packlift will refresh the repositories, reinstall the original package and recreate its shortcuts. It can also install missing libraries from the container repositories.").build();
                        confirm.add_button("Cancel",gtk::ResponseType::Cancel);
                        confirm.add_button("Repair",gtk::ResponseType::Accept);
                        let app = application.clone();
                        let record = record.clone();
                        confirm.connect_response(move |dialog, response| {
                            dialog.close();
                            if response == gtk::ResponseType::Accept {
                                let record = record.clone();
                                run_job(&app,&window,"Repairing installation",move |logger| installer_with_output(logger)?.repair_container_app(&record).map(Some));
                            }
                        });
                        confirm.present();
                    });
                    row.add_suffix(&repair);
                }
                {
                    let remove = gtk::Button::with_label(if game {
                        "Remove shortcut"
                    } else if method == "wine" {
                        "Remove from Packlift"
                    } else {
                        "Uninstall"
                    });
                    remove.set_valign(gtk::Align::Center);
                    let record_to_remove = record.clone();
                    let app = app.clone();
                    let weak_window = window.downgrade();
                    remove.connect_clicked(move |_| {
                        if let Err(error) =
                            installer().and_then(|manager| manager.uninstall(&record_to_remove))
                        {
                            if let Some(window) = weak_window.upgrade() {
                                show_error(&window, &error.to_string());
                            }
                        } else if let Some(window) = weak_window.upgrade() {
                            render_home(&app, &window);
                        }
                    });
                    row.add_suffix(&remove);
                }
                programs.add(&row);
            }
        }
        Err(error) => programs.set_description(Some(&error.to_string())),
    }
    content.append(&programs);
    let clean = gtk::Button::with_label("Clean unused environments");
    clean.set_halign(gtk::Align::Start);
    let app = app.clone();
    let weak_window = window.downgrade();
    clean.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            run_job(
                &app,
                &window,
                "Cleaning unused environments",
                move |logger| {
                    installer_with_output(logger)?
                        .clean_unused_environments()
                        .map(|_| None)
                },
            );
        }
    });
    content.append(&clean);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&content));
    toolbar.set_content(Some(&scroll));
    window.set_content(Some(&toolbar));
}

fn open_package(app: &adw::Application, path: &Path) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Packlift"));
    window.set_default_size(470, 440);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Package details", "Packlift")));
    toolbar.add_top_bar(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);

    match installer().and_then(|manager| manager.probe(path)) {
        Ok((package, plan)) => {
            let hero = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            hero.add_css_class("card");
            hero.set_margin_top(2);
            hero.set_margin_bottom(2);
            hero.set_margin_start(2);
            hero.set_margin_end(2);
            let image = package_image(&package, 56);
            let icon_frame = gtk::Frame::new(None);
            icon_frame.add_css_class("card");
            icon_frame.set_child(Some(&image));
            hero.append(&icon_frame);

            let identity = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let title = gtk::Label::new(Some(&package.name));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            title.set_wrap(true);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            identity.append(&title);
            let kind = gtk::Label::new(Some(&format!(
                "{} · {}",
                package.kind.to_uppercase(),
                package.architecture
            )));
            kind.add_css_class("dim-label");
            kind.set_xalign(0.0);
            identity.append(&kind);
            let version = if package.version.is_empty() {
                "Version unavailable".to_owned()
            } else {
                package.version.clone()
            };
            let bytes = std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let size = gtk::Label::new(Some(&format!("{version} · {}", human_size(bytes))));
            size.add_css_class("dim-label");
            size.set_xalign(0.0);
            identity.append(&size);
            hero.append(&identity);
            body.append(&hero);

            let verified = package.signature == "verified";
            let integrity = notice_row(
                if verified {
                    "emblem-ok-symbolic"
                } else {
                    "dialog-warning-symbolic"
                },
                if verified {
                    "Signature verified with a trusted key."
                } else {
                    "Signature could not be verified. Install only if you trust this file."
                },
                if verified { "success" } else { "warning" },
            );
            body.append(&integrity);

            let location = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let location_title = gtk::Label::new(Some("File and installation method"));
            location_title.add_css_class("heading");
            location_title.set_xalign(0.0);
            location.append(&location_title);
            let method = gtk::Label::new(Some(&format!(
                "{} · {}",
                plan.method.replace('-', " "),
                path.display()
            )));
            method.set_wrap(true);
            method.set_selectable(true);
            method.set_xalign(0.0);
            method.add_css_class("dim-label");
            location.append(&method);
            body.append(&location);

            let runtime = gtk::DropDown::from_strings(&["Wine", "Proton"]);
            if ["exe", "setup", "msi"].contains(&package.kind.as_str()) {
                if installer()
                    .map(|m| m.preferences().0 == "proton")
                    .unwrap_or(false)
                {
                    runtime.set_selected(1);
                }
                let runtime_box = gtk::Box::new(gtk::Orientation::Vertical, 7);
                let runtime_label = gtk::Label::new(Some("Windows runtime"));
                runtime_label.add_css_class("heading");
                runtime_label.set_xalign(0.0);
                runtime_box.append(&runtime_label);
                runtime.update_property(&[gtk::accessible::Property::Label("Windows runtime")]);
                runtime.set_hexpand(true);
                runtime_box.append(&runtime);
                body.append(&runtime_box);

                body.append(&notice_row(
                    "dialog-information-symbolic",
                    "Windows programs can access files in your home folder.",
                    "dim-label",
                ));
            }

            let libraries =
                gtk::CheckButton::with_label("Find and install missing libraries in the container");
            if plan.method == "toolbox" {
                libraries.set_active(true);
                body.append(&libraries);
            }
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            actions.set_halign(gtk::Align::End);
            let cancel = gtk::Button::with_label("Cancel");
            let weak_window = window.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
            });
            actions.append(&cancel);
            let rpm_target = gtk::DropDown::from_strings(&["Fedora", "openSUSE"]);
            let choose_target = package.kind == "rpm" && package.rpm_family.is_empty();
            if choose_target {
                body.append(&gtk::Label::new(Some(
                    "Choose the distribution this RPM was built for",
                )));
                rpm_target.update_property(&[gtk::accessible::Property::Label("RPM distribution")]);
                body.append(&rpm_target);
            }
            let setup = if plan.method == "toolbox" {
                installer().and_then(|m| m.container_setup())
            } else {
                Ok(None)
            };
            let install = gtk::Button::with_label(if matches!(&setup, Ok(Some(_))) {
                "Set up and install"
            } else {
                "Continue"
            });
            match &setup {
                Ok(command) if plan.method == "toolbox" => {
                    let mut summary = if choose_target {
                        "Install into the selected distribution container.".to_owned()
                    } else {
                        format!(
                            "Install in the {} container using {}.",
                            plan.family, plan.manager
                        )
                    };
                    if let Some(args) = command {
                        summary.push_str(&format!(
                            " Authorize this host setup command: {}",
                            args.join(" ")
                        ));
                        if matches!(args[0].as_str(), "rpm-ostree" | "transactional-update") {
                            summary.push_str(" Restart required.");
                        }
                    }
                    body.append(&notice_row(
                        "dialog-information-symbolic",
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
                        run_job(&app, &window, "Installing program or game", move |logger| {
                            installer_with_output(logger)?
                                .install_windows(&package, &runtime)
                                .map(Some)
                        });
                    } else {
                        run_job(&app, &window, "Installing program", move |logger| {
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
            body.append(&actions);
        }
        Err(error) => {
            let title = gtk::Label::new(Some("Could not open package"));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            body.append(&title);
            body.append(&notice_row(
                "dialog-error-symbolic",
                &error.to_string(),
                "error",
            ));
            let file = gtk::Label::new(Some(&path.display().to_string()));
            file.set_wrap(true);
            file.set_xalign(0.0);
            file.add_css_class("dim-label");
            body.append(&file);
        }
    }
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&body));
    toolbar.set_content(Some(&scroll));
    window.set_content(Some(&toolbar));
    window.present();
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
        .title("Choose how to use this .exe")
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
    let title = gtk::Label::new(Some("What is this .exe file?"));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    heading.append(&title);
    let subtitle = gtk::Label::new(Some(
        "Choose the option that matches the file you selected.",
    ));
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
        "Add an installed program or game",
        "Creates a menu shortcut for this .exe. Use this if the game or app is already installed.",
        false,
    );
    add.connect_clicked(move |_| {
        dialog_to_add.close();
        let package = package_to_add.clone();
        let runtime = runtime_to_add.clone();
        run_job(
            &app_to_add,
            &parent_to_add,
            "Adding program or game to menu",
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
        "Run this as an installer",
        "Use this if the .exe contains setup and needs to install the program first.",
        true,
    );
    install.connect_clicked(move |_| {
        dialog_to_install.close();
        let package = package_to_install.clone();
        let runtime = runtime_to_install.clone();
        run_job(
            &app_to_install,
            &parent_to_install,
            "Installing program or game",
            move |logger| {
                installer_with_output(logger)?
                    .install_windows(&package, &runtime)
                    .map(Some)
            },
        );
    });
    body.append(&install);

    let cancel = gtk::Button::with_label("Cancel");
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
    window.set_default_size(380, 220);
    window.set_resizable(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(14);
    body.set_margin_bottom(12);
    body.set_margin_start(20);
    body.set_margin_end(20);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("title-2");
    body.append(&heading);
    let stage = gtk::Label::new(Some("Preparing the installation environment…"));
    stage.set_wrap(true);
    stage.set_xalign(0.0);
    stage.add_css_class("dim-label");
    body.append(&stage);
    let progress = gtk::ProgressBar::new();
    progress.update_property(&[gtk::accessible::Property::Label("Installation progress")]);
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
    let copy = gtk::Button::with_label("Copy log");
    copy.set_halign(gtk::Align::End);
    let copy_log = log.clone();
    copy.connect_clicked(move |button| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&copy_log.borrow().join("\n"));
            button.set_label("Log copied");
        }
    });
    let details = gtk::Expander::builder().label("Show details").build();
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
                if label != "Working…" && stage.text().as_str() != label {
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
                        Err(error) => show_error(&window, &error.to_string()),
                        Ok(Some(record)) => show_success(&window, &record),
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
        "Game added to Packlift"
    } else {
        "Installation complete"
    }));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Center);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body.append(&title);
    let name = record["name"].as_str().unwrap_or("Program");
    let runnable = record["exports"][0]["argv"].is_array();
    let message = if runnable {
        format!("{name} is ready to use.")
    } else {
        format!("{name} is installed. This package has no application launcher.")
    };
    let description = gtk::Label::new(Some(&message));
    description.set_wrap(true);
    description.set_halign(gtk::Align::Center);
    description.add_css_class("dim-label");
    body.append(&description);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(4);
    let close = gtk::Button::with_label("Close");
    let weak_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    actions.append(&close);
    let open = gtk::Button::with_label("Open");
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
        .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-executable"));
    image.set_pixel_size(size);
    image
}

fn installation_stage(line: &str) -> (&'static str, f64) {
    let line = line.to_ascii_lowercase();
    if line.contains("analyzing") {
        ("Analyzing the package", 0.12)
    } else if line.contains("compatibility") {
        ("Checking compatibility", 0.25)
    } else if line.contains("environment") || line.contains("toolbox") || line.contains("prefix") {
        ("Preparing the installation environment", 0.38)
    } else if line.contains("download")
        || line.contains("flatpak install")
        || line.contains("pulling")
    {
        ("Downloading dependencies", 0.54)
    } else if line.contains("shortcut") || line.contains("menu") {
        ("Creating application shortcuts", 0.90)
    } else if line.contains("install") || line.contains("package") {
        ("Installing the program", 0.72)
    } else {
        ("Working…", 0.64)
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
        .text("Packlift could not complete the operation")
        .secondary_text(message)
        .build();
    dialog.add_button("Open log folder", gtk::ResponseType::Help);
    dialog.add_button("Copy log", gtk::ResponseType::Apply);
    dialog.connect_response(|dialog, response| {
        if response == gtk::ResponseType::Help {
            let folder = gio::File::for_path(packlift::system::data_home().join("flatapk"));
            let _ =
                gio::AppInfo::launch_default_for_uri(&folder.uri(), None::<&gio::AppLaunchContext>);
            return;
        }
        if response == gtk::ResponseType::Apply {
            let path = packlift::system::data_home()
                .join("flatapk")
                .join("flatapk.log");
            let contents = std::fs::read_to_string(path)
                .unwrap_or_else(|error| format!("Could not read the Packlift log: {error}"));
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&contents);
                dialog.set_secondary_text(Some("Log copied to clipboard."));
                return;
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn settings_dialog(window: &adw::ApplicationWindow) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some("Packlift settings"));
    dialog.set_transient_for(Some(window));
    dialog.set_modal(true);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Save", gtk::ResponseType::Accept);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let choices = gtk::DropDown::from_strings(&["Wine", "Proton"]);
    choices.update_property(&[gtk::accessible::Property::Label("Default Windows runtime")]);
    if installer()
        .map(|m| m.preferences().0 == "proton")
        .unwrap_or(false)
    {
        choices.set_selected(1);
    }
    content.append(&gtk::Label::new(Some("Default Windows runtime")));
    content.append(&choices);
    let logs = gtk::Button::with_label("View Packlift logs");
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
                    run_job(&app, &window, "Saving settings", task);
                }
            }
        }
    });
    dialog.present();
}

fn show_app_logs(parent: &adw::ApplicationWindow) {
    let window = gtk::Window::builder()
        .title("Packlift logs")
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
    let log_path = packlift::system::data_home()
        .join("flatapk")
        .join("flatapk.log");
    let log = std::rc::Rc::new(gtk::TextView::new());
    log.set_editable(false);
    log.set_cursor_visible(false);
    log.set_monospace(true);
    log.set_wrap_mode(gtk::WrapMode::WordChar);
    log.buffer().set_text(
        &std::fs::read_to_string(&log_path)
            .unwrap_or_else(|error| format!("No Packlift log is available yet. {error}")),
    );
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&*log)
        .build();
    body.append(&scroll);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let copy = gtk::Button::with_label("Copy all logs");
    let log_path_copy = log_path.clone();
    copy.connect_clicked(move |button| {
        if let Ok(contents) = std::fs::read_to_string(&log_path_copy) {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&contents);
                button.set_label("Logs copied");
            }
        }
    });
    actions.append(&copy);
    let close = gtk::Button::with_label("Close");
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
