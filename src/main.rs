use adw::prelude::*;
use gtk::prelude::*;
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc, thread, time::Duration};

use packlift::{
    core::{Package, PackliftError, Result},
    installer::Installer,
    system::LogHandler,
};

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
                let runtime = if record["runtime"].as_str().unwrap_or("") == "proton" {
                    "Proton"
                } else {
                    "Wine"
                };
                let row = adw::ActionRow::builder()
                    .title(name)
                    .subtitle(if game {
                        format!("Game shortcut · {runtime}")
                    } else {
                        "Installed program".into()
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
                    let remove =
                        gtk::Button::with_label(if game { "Remove shortcut" } else { "Uninstall" });
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
            let title = gtk::Label::new(Some(&package.name));
            title.add_css_class("title-2");
            title.set_xalign(0.0);
            body.append(&title);
            body.append(&gtk::Label::new(Some(&format!(
                "{} · {}",
                package.kind.to_uppercase(),
                package.architecture
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
                            .map(|_| ())
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
                        installer_with_output(logger)?.install_windows(&package, &runtime)
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
                                .map(|_| ())
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
    F: FnOnce(LogHandler) -> Result<()> + Send + 'static,
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
        Finished(Result<()>),
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
                log.borrow_mut().push(line);
                buffer.set_text(&log.borrow().join("\n"));
            }
            Ok(JobEvent::Finished(result)) => {
                if let Some(window) = weak_window.upgrade() {
                    if let Err(error) = result {
                        show_error(&window, &error.to_string());
                    } else {
                        render_home(&app, &window);
                    }
                }
                return glib::ControlFlow::Break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
        }
    });
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
            let task = move |logger| installer_with_output(logger)?.save_preferences(&runtime);
            if let Some(window) = weak_window.upgrade() {
                if let Some(app) = window.application().and_downcast::<adw::Application>() {
                    run_job(&app, &window, "Saving settings", task);
                }
            }
        }
    });
    dialog.present();
}
