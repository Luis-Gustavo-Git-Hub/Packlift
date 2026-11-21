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
                row.add_suffix(&launch);
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
    window.set_default_size(466, 410);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    match installer().and_then(|manager| manager.probe(path)) {
        Ok((package, plan)) => {
            if let Some(icon) = package.icon.as_ref().filter(|path| path.is_file()) {
                let image = gtk::Image::from_file(icon);
                image.set_pixel_size(48);
                body.append(&image);
            }
            let title = gtk::Label::new(Some(&package.name));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            body.append(&title);
            body.append(&gtk::Label::new(Some(&format!(
                "{} · {}",
                package.kind.to_uppercase(),
                package.architecture
            ))));
            let version = if package.version.is_empty() {
                "Version unavailable".to_owned()
            } else {
                package.version.clone()
            };
            let bytes = std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            body.append(&gtk::Label::new(Some(&format!(
                "{version} · {}",
                human_size(bytes)
            ))));
            let integrity = if package.signature == "verified" {
                gtk::Label::new(Some("Signature verified with a trusted key."))
            } else {
                gtk::Label::new(Some(
                    "Signature could not be verified. Install only if you trust the source.",
                ))
            };
            integrity.set_wrap(true);
            integrity.set_xalign(0.0);
            integrity.add_css_class(if package.signature == "verified" {
                "success"
            } else {
                "warning"
            });
            body.append(&integrity);
            let method = gtk::Label::new(Some(&format!("{}\n{}", plan.method, path.display())));
            method.set_wrap(true);
            method.set_xalign(0.0);
            body.append(&method);
            let runtime = gtk::DropDown::from_strings(&["Wine", "Proton (automatic install)"]);
            if ["exe", "setup", "msi"].contains(&package.kind.as_str()) {
                if installer()
                    .map(|m| m.preferences().0 == "proton")
                    .unwrap_or(false)
                {
                    runtime.set_selected(1);
                }
                body.append(&runtime);
                let note = gtk::Label::new(Some(
                    "Packlift downloads UMU-Proton automatically the first time you use it.",
                ));
                note.add_css_class("dim-label");
                note.set_wrap(true);
                body.append(&note);
                let isolation = gtk::Label::new(Some(
                    "Windows programs can access your home folder while running through Wine or Proton.",
                ));
                isolation.add_css_class("dim-label");
                isolation.set_wrap(true);
                body.append(&isolation);
            }
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            actions.set_halign(gtk::Align::End);
            let cancel = gtk::Button::with_label("Cancel");
            let weak_window = window.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
            });
            actions.append(&cancel);
            let install = gtk::Button::with_label("Install");
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
                if kind == "exe" || kind == "setup" {
                    if let Some(window) = weak_window.upgrade() {
                        choose_windows_action(&app, &window, &package, &runtime);
                    }
                } else if let Some(window) = weak_window.upgrade() {
                    let package = package.clone();
                    let runtime = runtime.clone();
                    run_job(&app, &window, "Installing program", move |logger| {
                        installer_with_output(logger)?
                            .install_package(&package, &runtime)
                            .map(Some)
                    });
                }
            });
            actions.append(&install);
            body.append(&actions);
        }
        Err(error) => {
            let title = gtk::Label::new(Some("Could not open package"));
            title.add_css_class("title-2");
            body.append(&title);
            let message = gtk::Label::new(Some(&error.to_string()));
            message.set_wrap(true);
            body.append(&message);
            body.append(&gtk::Label::new(Some(&path.display().to_string())));
        }
    }
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&body));
    window.set_content(Some(&scroll));
    window.present();
}

fn choose_windows_action(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    package: &Package,
    runtime: &str,
) {
    let dialog = gtk::Dialog::new();
    dialog.set_title(Some("How do you want to use this .exe?"));
    dialog.set_transient_for(Some(window));
    dialog.set_modal(true);
    dialog.add_button("Install program or game", gtk::ResponseType::Accept);
    dialog.add_button(
        "Add installed game to Packlift and menu",
        gtk::ResponseType::Apply,
    );
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    let message = gtk::Label::new(Some("Run a Windows installer, or add an already installed game to Packlift and the applications menu."));
    message.set_wrap(true);
    dialog.content_area().append(&message);
    let package = package.clone();
    let runtime = runtime.to_owned();
    let app = app.clone();
    let weak_window = window.downgrade();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if let Some(window) = weak_window.upgrade() {
            match response {
                gtk::ResponseType::Accept => {
                    let package = package.clone();
                    let runtime = runtime.clone();
                    run_job(&app, &window, "Installing program or game", move |logger| {
                        installer_with_output(logger)?
                            .install_windows(&package, &runtime)
                            .map(Some)
                    });
                }
                gtk::ResponseType::Apply => {
                    let package = package.clone();
                    let runtime = runtime.clone();
                    run_job(
                        &app,
                        &window,
                        "Adding game to Packlift and menu",
                        move |logger| {
                            installer_with_output(logger)?
                                .add_game(&package, &runtime)
                                .map(Some)
                        },
                    );
                }
                _ => {}
            }
        }
    });
    dialog.present();
}

fn run_job<F>(app: &adw::Application, window: &adw::ApplicationWindow, title: &str, task: F)
where
    F: FnOnce(LogHandler) -> Result<Option<Value>> + Send + 'static,
{
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(30);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("title-2");
    body.append(&heading);
    let stage = gtk::Label::new(Some("Preparing the installation environment…"));
    stage.set_xalign(0.0);
    stage.add_css_class("dim-label");
    body.append(&stage);
    let progress = gtk::ProgressBar::new();
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
    copy.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&copy_log.borrow().join("\n"));
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
                stage.set_text(label);
                progress.set_fraction(fraction);
                log.borrow_mut().push(line);
                buffer.set_text(&log.borrow().join("\n"));
            }
            Ok(JobEvent::Finished(result)) => {
                progress.set_fraction(if result.is_ok() { 1.0 } else { 0.0 });
                if let Some(window) = weak_window.upgrade() {
                    match result {
                        Err(error) => show_error(&window, &error.to_string()),
                        Ok(Some(record)) => show_success(&app, &window, &record),
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

fn show_success(app: &adw::Application, window: &adw::ApplicationWindow, record: &Value) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(30);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("success");
    body.append(&icon);
    let title = gtk::Label::new(Some("Installation complete"));
    title.add_css_class("title-2");
    body.append(&title);
    let name = record["name"].as_str().unwrap_or("Program");
    let description = gtk::Label::new(Some(&format!("{name} is ready in Packlift.")));
    description.set_wrap(true);
    body.append(&description);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let close = gtk::Button::with_label("Close");
    let close_app = app.clone();
    let weak_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            render_home(&close_app, &window);
        }
    });
    actions.append(&close);
    let open = gtk::Button::with_label("Open program");
    open.add_css_class("suggested-action");
    let record = record.clone();
    let open_app = app.clone();
    let weak_window = window.downgrade();
    open.connect_clicked(move |_| {
        if let Err(error) = installer().and_then(|manager| manager.launch(&record)) {
            if let Some(window) = weak_window.upgrade() {
                show_error(&window, &error.to_string());
            }
        } else if let Some(window) = weak_window.upgrade() {
            render_home(&open_app, &window);
        }
    });
    actions.append(&open);
    body.append(&actions);
    window.set_content(Some(&body));
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
    dialog.connect_response(|dialog, _| dialog.close());
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
    let choices = gtk::DropDown::from_strings(&["Wine", "Proton (automatic install)"]);
    if installer()
        .map(|m| m.preferences().0 == "proton")
        .unwrap_or(false)
    {
        choices.set_selected(1);
    }
    content.append(&gtk::Label::new(Some("Default Windows runtime")));
    content.append(&choices);
    content.append(&gtk::Label::new(Some(
        "Packlift downloads UMU-Proton automatically when you first prepare a game.",
    )));
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
