mod flatpak_ui;
mod i18n;
mod store_ui;

use adw::prelude::*;
use i18n::tr;
use std::{path::Path, rc::Rc, sync::Arc, thread, time::Duration};

use packlift::{
    core::{Package, PackliftError, Result},
    installer::{windows_prefix_drive, Installer},
    system::{host_access_available, packlift_data_dir, LogHandler},
    APP_ID,
};
use serde_json::Value;

fn show_host_access_dialog(parent: &adw::ApplicationWindow) {
    let command = format!("flatpak override --user --talk-name=org.freedesktop.Flatpak {APP_ID}");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
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
    let copy = text_button(tr("Copy command"));
    copy.add_css_class("suggested-action");
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&command);
        button.set_label(tr("Command copied"));
        button.announce(
            tr("Command copied"),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    });
    show_editor(parent, tr("Allow access to your system"), &content, &copy);
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|_| {
        if let Some(display) = gtk::gdk::Display::default() {
            let css = gtk::CssProvider::new();
            css.load_from_data(
                ".packlift-home-hero { padding: 20px; border-radius: 16px; background-color: alpha(@accent_bg_color, 0.12); border: 1px solid alpha(@accent_bg_color, 0.25); }
                 .packlift-home-icon { color: @accent_color; padding: 10px; border-radius: 12px; background-color: alpha(@accent_bg_color, 0.15); }
                 .packlift-package-hero { padding: 16px; border-radius: 14px; background-image: linear-gradient(125deg, alpha(@accent_bg_color, 0.17), alpha(@accent_bg_color, 0.04)); border: 1px solid alpha(@accent_bg_color, 0.22); }
                 .packlift-package-icon { padding: 9px; border-radius: 12px; background-color: alpha(@card_bg_color, 0.8); }
                 .packlift-package-note { padding: 10px 12px; border-radius: 9px; background-color: @card_bg_color; }
                 .packlift-progress { padding: 14px 16px; border-radius: 12px; background: @card_bg_color; box-shadow: 0 4px 18px alpha(black, 0.24); }
                 .packlift-store-feature { padding: 20px 24px; border-radius: 18px; background-color: @card_bg_color; background-image: linear-gradient(120deg, alpha(@accent_bg_color, 0.18), alpha(@accent_bg_color, 0.04)); border: 1px solid alpha(@accent_bg_color, 0.23); }
                 .packlift-store-preview { border-radius: 10px; box-shadow: 0 3px 12px alpha(black, 0.2); }
                 button.packlift-scope-option { padding: 8px 10px; border-radius: 10px; background: alpha(@window_fg_color, 0.07); }
                 button.packlift-scope-option:hover, button.packlift-scope-option:focus-visible { background: alpha(@accent_bg_color, 0.19); }
                 .packlift-scope-icon { padding: 7px; border-radius: 8px; color: @accent_color; background: alpha(@accent_bg_color, 0.15); }
                 .packlift-store-detail-hero { padding: 18px 20px; border-radius: 16px; background-color: @card_bg_color; background-image: linear-gradient(125deg, alpha(@accent_bg_color, 0.12), transparent); border: 1px solid alpha(@borders, 0.5); }
                 .packlift-store-fact > button { padding: 5px 10px; border-radius: 8px; background: alpha(@window_fg_color, 0.07); }
                 .packlift-store-fact > button:hover, .packlift-store-fact > button:checked { background: alpha(@accent_bg_color, 0.19); }
                 .packlift-store-shot { border-radius: 10px; }
                 button.packlift-store-shot-button { padding: 0; border-radius: 10px; }
                 .packlift-store-zoom { margin: 8px; padding: 6px; border-radius: 8px; background: @card_bg_color; }
                 .packlift-store-release { padding: 16px; border-radius: 12px; background: @card_bg_color; }
                 button.packlift-store-card { padding: 12px; border-radius: 12px; background: @card_bg_color; border: 1px solid alpha(@borders, 0.55); }
                 button.packlift-store-card:hover { background: alpha(@accent_bg_color, 0.13); }",
            );
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    let open_flatpak = gio::SimpleAction::new(
        "open-installed-flatpak",
        Some(&<(String, String)>::static_variant_type()),
    );
    let application = app.downgrade();
    open_flatpak.connect_activate(move |_, parameter| {
        let Some((scope, target)) = parameter.and_then(|value| value.get::<(String, String)>())
        else {
            return;
        };
        let Some(app) = application.upgrade() else {
            return;
        };
        let hold = app.hold();
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let result = gio::spawn_blocking(move || {
                let manager = installer()?;
                let id = target.split('/').next().unwrap_or_default();
                let record = manager
                    .flatpak_app_installations(id)?
                    .into_iter()
                    .find(|record| {
                        record["flatpak_scope"].as_str() == Some(&scope)
                            && record["target"].as_str() == Some(&target)
                    })
                    .ok_or_else(|| {
                        PackliftError::Message(
                            tr("The selected Flatpak installation was no longer found.").into(),
                        )
                    })?;
                manager.launch(&record)
            })
            .await;
            if let Ok(Err(error)) = result {
                build_home(&app);
                if let Some(window) = app.active_window().and_downcast::<adw::ApplicationWindow>() {
                    show_error(&window, &error.to_string());
                }
            }
        });
    });
    app.add_action(&open_flatpak);
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
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Packlift"));
    window.set_default_size(720, 540);
    render_home(app, &window);
    window.present();
}

fn render_home(app: &adw::Application, window: &adw::ApplicationWindow) {
    window.set_title(Some("Packlift"));
    window.set_widget_name("packlift-library");
    window.set_resizable(true);
    let host_access = host_access_available();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Packlift", "")));

    let (menu, popover, actions) = action_menu(tr("Packlift menu"), "open-menu-symbolic");
    let store = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("system-software-install-symbolic")
                .label(tr("Store"))
                .build(),
        )
        .build();
    let store_app = app.clone();
    let parent = window.downgrade();
    store.connect_clicked(move |_| {
        if let Some(parent) = parent.upgrade() {
            store_ui::show(&store_app, &parent);
        }
    });
    header.pack_start(&store);
    let library_app = app.clone();
    let parent = window.downgrade();
    add_menu_item(&popover, &actions, tr("Refresh library"), move || {
        if let Some(window) = parent.upgrade() {
            render_home(&library_app, &window);
        }
    });
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
        let parent = window.downgrade();
        dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    if let Some(window) = parent.upgrade() {
                        review_package(&app, &window, &path);
                    }
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
    match installer().and_then(|manager| manager.library_apps()) {
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
            search.set_hexpand(true);
            let search_focus = search.downgrade();
            page_shortcut(&toolbar, "<Control>f", move || {
                if let Some(search) = search_focus.upgrade() {
                    search.grab_focus();
                }
            });
            search.update_property(&[gtk::accessible::Property::Label(tr(
                "Search programs and games",
            ))]);
            let filters = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            filters.append(&search);
            let labels: Vec<_> = LIBRARY_FILTERS.iter().map(|(label, _)| tr(label)).collect();
            let scope = gtk::DropDown::from_strings(&labels);
            scope.update_property(&[gtk::accessible::Property::Label(tr("Filter by app type"))]);
            filters.append(&scope);
            library.append(&filters);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("boxed-list");
            let rows: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let row = library_row(app, window, &record);
                    let searchable = format!(
                        "{} {} {}",
                        row.title(),
                        row.subtitle().unwrap_or_default(),
                        record["target"].as_str().unwrap_or_default()
                    )
                    .to_lowercase();
                    list.append(&row);
                    (row, searchable, record)
                })
                .collect();
            library.append(&list);
            let empty = library_status(
                "system-search-symbolic",
                tr("No matches"),
                tr("Try a different search or filter."),
            );
            empty.set_visible(false);
            library.append(&empty);
            let update_filter = Rc::new(move |query: &str, scope: u32| {
                let query = query.trim().to_lowercase();
                let filter = LIBRARY_FILTERS
                    .get(scope as usize)
                    .unwrap_or(&LIBRARY_FILTERS[0])
                    .1;
                let mut matches = false;
                for (row, searchable, record) in &rows {
                    let visible = searchable.contains(&query) && filter(record);
                    row.set_visible(visible);
                    matches |= visible;
                }
                list.set_visible(matches);
                empty.set_visible(!matches);
            });
            let search_filter = update_filter.clone();
            let scope_ref = scope.downgrade();
            search.connect_search_changed(move |entry| {
                search_filter(
                    &entry.text(),
                    scope_ref.upgrade().map_or(0, |scope| scope.selected()),
                );
            });
            let search = search.downgrade();
            scope.connect_selected_notify(move |scope| {
                if let Some(search) = search.upgrade() {
                    update_filter(&search.text(), scope.selected());
                }
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
    store.grab_focus();
}

#[derive(Clone)]
struct PageReturn {
    content: gtk::Widget,
    title: Option<glib::GString>,
    name: glib::GString,
    focus: Option<gtk::Widget>,
}

impl PageReturn {
    fn capture(window: &adw::ApplicationWindow) -> Option<Self> {
        Some(Self {
            content: window.content()?,
            title: window.title(),
            name: window.widget_name(),
            focus: gtk::prelude::GtkWindowExt::focus(window),
        })
    }

    fn restore(&self, window: &adw::ApplicationWindow) {
        window.set_title(self.title.as_deref());
        window.set_widget_name(&self.name);
        window.set_content(Some(&self.content));
        if let Some(focus) = &self.focus {
            focus.grab_focus();
        }
    }
}

fn navigation_button(label: &str, icon: &str) -> gtk::Button {
    gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(icon)
                .label(label)
                .build(),
        )
        .build()
}

fn page_shortcut(widget: &impl IsA<gtk::Widget>, accelerator: &str, activate: impl Fn() + 'static) {
    let Some(trigger) = gtk::ShortcutTrigger::parse_string(accelerator) else {
        return;
    };
    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Managed);
    controller.add_shortcut(gtk::Shortcut::new(
        Some(trigger),
        Some(gtk::CallbackAction::new(move |_, _| {
            activate();
            glib::Propagation::Stop
        })),
    ));
    widget.add_controller(controller);
}

fn page_navigation(
    toolbar: &adw::ToolbarView,
    header: &adw::HeaderBar,
    label: &str,
    icon: &str,
    back: impl Fn() + 'static,
) {
    let button = navigation_button(label, icon);
    header.pack_start(&button);
    let back = Rc::new(back);
    let shortcut = back.clone();
    page_shortcut(toolbar, "<Alt>Left", move || shortcut());
    button.connect_clicked(move |_| back());
}

fn back_to_previous(
    toolbar: &adw::ToolbarView,
    header: &adw::HeaderBar,
    window: &adw::ApplicationWindow,
) -> Option<PageReturn> {
    let previous = PageReturn::capture(window)?;
    let page = previous.clone();
    let window = window.downgrade();
    page_navigation(
        toolbar,
        header,
        tr("Back"),
        "go-previous-symbolic",
        move || {
            if let Some(window) = window.upgrade() {
                page.restore(&window);
            }
        },
    );
    Some(previous)
}

type LibraryFilter = (&'static str, fn(&Value) -> bool);

const LIBRARY_FILTERS: &[LibraryFilter] = &[
    ("All apps", |_| true),
    ("Flatpak apps", |app| app["method"] == "flatpak"),
    ("User Flatpaks", |app| {
        app["method"] == "flatpak" && app["flatpak_scope"].as_str().unwrap_or("user") == "user"
    }),
    ("System Flatpaks", |app| {
        app["method"] == "flatpak"
            && app["flatpak_scope"]
                .as_str()
                .is_some_and(|scope| scope != "user")
    }),
    ("Windows apps", |app| app["method"] == "wine"),
    ("Windows game shortcuts", |app| {
        app["method"] == "wine" && app["game_shortcut"] == true
    }),
    ("Wine", |app| {
        app["method"] == "wine" && app["runtime"] != "proton"
    }),
    ("Proton", |app| {
        app["method"] == "wine" && app["runtime"] == "proton"
    }),
    ("Local Linux apps", |app| app["method"] == "local"),
    ("Native Linux packages", |app| app["method"] == "native"),
    ("Toolbx apps", |app| app["method"] == "toolbox"),
    ("Other apps", |app| app["method"] != "flatpak"),
];

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
        "flatpak" => flatpak_scope_label(record),
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
    if method == "flatpak" {
        let parent = window.downgrade();
        let record = record.clone();
        add_menu_item(&popover, &actions, tr("Permissions…"), move || {
            if let Some(window) = parent.upgrade() {
                flatpak_ui::show(&window, &record);
            }
        });
    }
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
            tr("The game files will stay in their original folder.").into(),
        ),
        (_, "flatpak") => (
            tr("Uninstall"),
            tr("You are removing this installation ({scope}).")
                .replace("{scope}", &flatpak_scope_label(record)),
        ),
        (_, "wine") => (
            tr("Remove from Packlift"),
            tr("This removes the program's library entry, shortcut and managed installation files.").into(),
        ),
        _ => (
            tr("Uninstall"),
            tr("This removes the installed program and its shortcuts.").into(),
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
        let dialog = library_action_dialog(
            &window,
            &format!("{remove}: {name}?"),
            &detail,
            remove,
            true,
        );
        let delete_data = (record["method"] == "flatpak").then(|| {
            let (row, check) = flatpak_delete_data_option();
            if let Ok(area) = dialog.message_area().downcast::<gtk::Box>() {
                area.append(&row);
            }
            check
        });
        let parent = window.downgrade();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Accept {
                return;
            }
            let Some(window) = parent.upgrade() else {
                return;
            };
            let record = record.clone();
            let delete_data = delete_data
                .as_ref()
                .is_some_and(gtk::CheckButton::is_active);
            run_job(
                &app,
                &window,
                tr("Removing from your library"),
                move |logger| {
                    let manager = installer_with_output(logger)?;
                    if delete_data {
                        manager.uninstall_removing_data(&record)?;
                    } else {
                        manager.uninstall(&record)?;
                    }
                    Ok(None)
                },
            );
        });
        dialog.present();
    });
    row.add_suffix(&menu);
    row
}

fn flatpak_delete_data_option() -> (gtk::Box, gtk::CheckButton) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let check = gtk::CheckButton::new();
    check.set_child(Some(&home_label(tr("Delete app data"), "heading")));
    row.append(&check);
    let note = home_label(
        tr("This permanently deletes saved data and settings for this app in your account, including data shared with other installations of the same app."),
        "dim-label",
    );
    note.set_max_width_chars(46);
    note.set_visible(false);
    row.append(&note);
    check.connect_toggled(move |check| note.set_visible(check.is_active()));
    (row, check)
}

fn flatpak_scope_label(record: &Value) -> String {
    match record["flatpak_scope"].as_str() {
        Some("system") => tr("System Flatpak").into(),
        Some(scope) if scope.starts_with("system (") => {
            format!("{} · {}", tr("System Flatpak"), &scope["system ".len()..])
        }
        _ => tr("Flatpak for your account").into(),
    }
}

fn confirm_library_action(
    window: &adw::ApplicationWindow,
    title: &str,
    description: &str,
    label: &str,
    destructive: bool,
    action: impl Fn(&adw::ApplicationWindow) + 'static,
) {
    let dialog = library_action_dialog(window, title, description, label, destructive);
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

fn library_action_dialog(
    window: &adw::ApplicationWindow,
    title: &str,
    description: &str,
    label: &str,
    destructive: bool,
) -> gtk::MessageDialog {
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
    dialog
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
    let title = tr("Runtime for {name}").replace("{name}", name);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
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
    let apply = text_button(tr("Apply"));
    apply.add_css_class("suggested-action");
    apply.connect_clicked(move |_| {
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
    show_editor(window, &title, &body, &apply);
}

fn open_package(app: &adw::Application, path: &Path) {
    let window = app
        .active_window()
        .and_downcast::<adw::ApplicationWindow>()
        .filter(|window| {
            window.widget_name() == "packlift-library" || window.widget_name() == "packlift-store"
        })
        .filter(|window| !window.has_css_class("packlift-running-job"))
        .unwrap_or_else(|| {
            let window = adw::ApplicationWindow::new(app);
            window.set_default_size(470, -1);
            window
        });
    review_package(app, &window, path);
}

fn review_package(app: &adw::Application, window: &adw::ApplicationWindow, path: &Path) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Review package"), "")));
    let previous = back_to_previous(&toolbar, &header, window);
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
                    if let Some(page) = &previous {
                        page.restore(&window);
                    } else {
                        window.close();
                    }
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
    window.set_widget_name("packlift-package-review");
    window.set_title(Some(tr("Review package")));
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
    let flatpak = matches!(package.kind.as_str(), "flatpak" | "flatpakref");
    let texture = texture.or_else(|| {
        (flatpak && package.identifier == APP_ID)
            .then(|| {
                let bytes = glib::Bytes::from_static(include_bytes!(
                    "../data/io.github.luis_gustavo_git_hub.Packlift.png"
                ));
                gtk::gdk::Texture::from_bytes(&bytes).ok()
            })
            .flatten()
    });
    let image = if let Some(texture) = texture {
        gtk::Image::from_paintable(Some(&texture))
    } else if flatpak {
        let has_icon = gtk::gdk::Display::default().is_some_and(|display| {
            gtk::IconTheme::for_display(&display).has_icon(&package.identifier)
        });
        gtk::Image::from_icon_name(if has_icon {
            &package.identifier
        } else {
            "system-software-install-symbolic"
        })
    } else {
        gtk::Image::from_icon_name("application-x-executable")
    };
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
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        tr("Choose how to use this .exe"),
        "",
    )));
    let previous = back_to_previous(&toolbar, &header, parent);
    toolbar.add_top_bar(&header);

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

    let add = choice_button(
        tr("Add an installed program or game"),
        tr("Creates a menu shortcut for this .exe. Use this if the game or app is already installed."),
        false,
    );
    add.connect_clicked(move |_| {
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

    let install = choice_button(
        tr("Run this as an installer"),
        tr("Use this if the .exe contains setup and needs to install the program first."),
        true,
    );
    install.connect_clicked(move |_| {
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
    let window = parent.downgrade();
    cancel.connect_clicked(move |_| {
        if let (Some(window), Some(previous)) = (window.upgrade(), previous.as_ref()) {
            previous.restore(&window);
        }
    });
    body.append(&cancel);
    toolbar.set_content(Some(&body));
    parent.set_content(Some(&toolbar));
    add.grab_focus();
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
    if window.has_css_class("packlift-running-job") {
        return;
    }
    window.add_css_class("packlift-running-job");
    let previous = PageReturn::capture(window);
    let store_toolbar = if window.widget_name() == "packlift-store-details" {
        window.content().and_downcast::<adw::ToolbarView>()
    } else {
        None
    };
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(20);
    body.set_margin_end(20);
    if store_toolbar.is_some() {
        body.set_margin_top(8);
        body.set_margin_bottom(10);
    } else {
        body.add_css_class("packlift-progress");
        body.set_halign(gtk::Align::Center);
        body.set_valign(gtk::Align::Center);
        body.set_size_request(280, -1);
        body.set_margin_top(20);
        body.set_margin_bottom(20);
    }
    let heading = gtk::Label::new(Some(title));
    heading.set_wrap(true);
    heading.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    if store_toolbar.is_some() {
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
    } else {
        heading.add_css_class("title-2");
    }
    body.append(&heading);
    let stage = gtk::Label::new(Some(tr("Working…")));
    stage.set_wrap(true);
    stage.set_xalign(0.0);
    stage.add_css_class("dim-label");
    stage.set_hexpand(true);
    let status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spinner = gtk::Spinner::new();
    spinner.start();
    status.append(&spinner);
    status.append(&stage);
    body.append(&status);
    let progress = gtk::ProgressBar::new();
    progress.update_property(&[gtk::accessible::Property::Label(tr(
        "Installation progress",
    ))]);
    progress.set_show_text(false);
    progress.set_fraction(0.08);
    body.append(&progress);
    let store_progress = store_toolbar.map(|toolbar| {
        let footer = adw::Clamp::builder().maximum_size(560).child(&body).build();
        toolbar.set_bottom_bar_style(adw::ToolbarStyle::RaisedBorder);
        toolbar.add_bottom_bar(&footer);
        (toolbar, footer)
    });
    if store_progress.is_none() {
        window.set_content(Some(&body));
    }
    stage.announce(title, gtk::AccessibleAnnouncementPriority::Low);

    enum JobEvent {
        Progress(&'static str, f64),
        Finished(Result<Option<Value>>),
    }
    let (sender, receiver) = std::sync::mpsc::channel::<JobEvent>();
    thread::spawn(move || {
        let log_sender = sender.clone();
        let logger: LogHandler = Arc::new(move |line| {
            let (stage, fraction) = installation_stage(line);
            if stage != tr("Working…") {
                let _ = log_sender.send(JobEvent::Progress(stage, fraction));
            }
        });
        let _ = sender.send(JobEvent::Finished(task(logger)));
    });
    let app = app.clone();
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(120), move || loop {
        match receiver.try_recv() {
            Ok(JobEvent::Progress(label, fraction)) => {
                if stage.text().as_str() != label {
                    stage.set_text(label);
                    stage.announce(label, gtk::AccessibleAnnouncementPriority::Low);
                }
                progress.set_fraction(progress.fraction().max(fraction));
            }
            Ok(JobEvent::Finished(result)) => {
                if let Ok(Some(record)) = &result {
                    if record["method"] == "flatpak" {
                        if record["flatpak_already_installed"].as_bool() != Some(true) {
                            notify_flatpak_installed(&app, record);
                        }
                        if record["flatpak_app_id"].as_str() == Some(APP_ID) {
                            for window in app.windows() {
                                window.destroy();
                            }
                            app.quit();
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                progress.set_fraction(if result.is_ok() { 1.0 } else { 0.0 });
                if let Some((toolbar, footer)) = &store_progress {
                    toolbar.remove(footer);
                }
                if let Some(window) = weak_window.upgrade() {
                    window.remove_css_class("packlift-running-job");
                    let left_store_page = store_progress.as_ref().is_some_and(|(toolbar, _)| {
                        window.content().as_ref() != Some(toolbar.upcast_ref())
                    });
                    if let Some(page) = previous.as_ref().filter(|_| !left_store_page) {
                        if store_progress.is_some() {
                            // Remap the details to refresh its installed state after the job.
                            window.set_content(None::<&gtk::Widget>);
                        }
                        page.restore(&window);
                    }
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
                        Err(error) => show_error(&window, &error.to_string()),
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
                            if record["method"] == "flatpak" {
                                let page = window.widget_name();
                                if page == "packlift-library"
                                    || (!left_store_page && page != "packlift-store-details")
                                {
                                    render_home(&app, &window);
                                }
                            } else {
                                show_success(&app, &window, &record);
                            }
                        }
                        Ok(None)
                            if store_progress.is_some()
                                && window.widget_name() != "packlift-library" => {}
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
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        tr("Choose the installed executable"),
        "",
    )));
    back_to_previous(&toolbar, &header, window);
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
    choose.grab_focus();
}

fn notify_flatpak_installed(app: &adw::Application, record: &Value) {
    let notification = gio::Notification::new(tr("Installation complete"));
    let name = record["name"].as_str().unwrap_or(tr("Program"));
    notification.set_body(Some(&tr("{name} was installed.").replace("{name}", name)));
    if let (Some(target), Some(scope)) =
        (record["target"].as_str(), record["flatpak_scope"].as_str())
    {
        let action = "app.open-installed-flatpak";
        let parameter = (scope, target).to_variant();
        notification.add_button_with_target_value(tr("Open"), action, Some(&parameter));
        notification.set_default_action_and_target_value(action, Some(&parameter));
    }
    app.send_notification(None, &notification);
}

fn show_success(app: &adw::Application, window: &adw::ApplicationWindow, record: &Value) {
    let app = app.clone();
    let target = window.downgrade();
    let finish = Rc::new(move || {
        if let Some(window) = target.upgrade() {
            render_home(&app, &window);
        }
    });
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
    let close_page = finish.clone();
    close.connect_clicked(move |_| close_page());
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
        } else {
            finish();
        }
    });
    if runnable {
        actions.append(&open);
    }
    body.append(&actions);
    window.set_content(Some(&body));
    close.grab_focus();
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
            let named_icon = !icon.is_empty() && !Path::new(icon).is_absolute();
            let missing_flatpak_icon = record["method"] == "flatpak"
                && gtk::gdk::Display::default()
                    .is_some_and(|display| !gtk::IconTheme::for_display(&display).has_icon(icon));
            gtk::Image::from_icon_name(if named_icon && !missing_flatpak_icon {
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
    if line.contains("uninstall") || line.contains("removing ") {
        (tr("Uninstalling the program"), 0.72)
    } else if line.contains("analyzing") {
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
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Packlift", "")));
    back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.set_valign(gtk::Align::Start);
    body.set_margin_top(24);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_bottom(20);
    let title = tr("Packlift could not complete the operation");
    body.append(&home_label(title, "title-2"));
    let detail = home_label(message, "dim-label");
    detail.set_selectable(true);
    body.append(&detail);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let copy = text_button(tr("Copy log"));
    let directory = text_button(tr("Open log folder"));
    directory.connect_clicked(|_| {
        let folder = gio::File::for_path(packlift_data_dir());
        let _ = gio::AppInfo::launch_default_for_uri(&folder.uri(), None::<&gio::AppLaunchContext>);
    });
    copy.connect_clicked(|button| {
        let path = packlift_data_dir().join("flatapk.log");
        let contents = std::fs::read_to_string(path).unwrap_or_else(|error| {
            tr("Could not read the Packlift log: {error}").replace("{error}", &error.to_string())
        });
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&contents);
            button.set_label(tr("Log copied to clipboard."));
        }
    });
    actions.append(&copy);
    actions.append(&directory);
    body.append(&actions);
    let clamp = adw::Clamp::builder().maximum_size(640).child(&body).build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    toolbar.set_content(Some(&scroll));
    window.set_widget_name("packlift-error");
    window.set_content(Some(&toolbar));
    detail.announce(title, gtk::AccessibleAnnouncementPriority::Medium);
    copy.grab_focus();
}

fn settings_dialog(window: &adw::ApplicationWindow) {
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
    let save = text_button(tr("Save"));
    save.add_css_class("suggested-action");
    let weak_window = window.downgrade();
    save.connect_clicked(move |_| {
        let runtime = if choices.selected() == 1 {
            "proton"
        } else {
            "wine"
        };
        if let Some(window) = weak_window.upgrade() {
            if let Some(app) = window.application().and_downcast::<adw::Application>() {
                run_job(&app, &window, tr("Saving settings"), move |logger| {
                    installer_with_output(logger)?.save_preferences(runtime)?;
                    Ok(None)
                });
            }
        }
    });
    show_editor(window, tr("Packlift settings"), &content, &save);
}

fn show_editor(
    window: &adw::ApplicationWindow,
    title: &str,
    body: &gtk::Box,
    action: &gtk::Button,
) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, "")));
    back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);
    action.set_halign(gtk::Align::End);
    body.append(action);
    body.set_valign(gtk::Align::Start);
    let clamp = adw::Clamp::builder().maximum_size(620).child(body).build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    toolbar.set_content(Some(&scroll));
    window.set_content(Some(&toolbar));
    body.child_focus(gtk::DirectionType::TabForward);
}

fn show_app_logs(parent: &adw::ApplicationWindow) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Packlift logs"), "")));
    back_to_previous(&toolbar, &header, parent);
    toolbar.add_top_bar(&header);
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
    body.append(&actions);
    toolbar.set_content(Some(&body));
    parent.set_content(Some(&toolbar));
    copy.grab_focus();
}
