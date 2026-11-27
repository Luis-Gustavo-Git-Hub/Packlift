mod flatpak_ui;
mod i18n;
mod operations_ui;
mod search_provider;
mod store_ui;
mod updates_ui;

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
            css.load_from_data(&[
                "scrollbar.vertical { opacity: 0; }
                 .packlift-home-hero { padding: 20px; border-radius: 16px; background-color: alpha(@accent_bg_color, 0.12); border: 1px solid alpha(@accent_bg_color, 0.25); }
                 .packlift-home-icon { color: @accent_color; padding: 10px; border-radius: 12px; background-color: alpha(@accent_bg_color, 0.15); }
                 .packlift-package-hero { padding: 16px; border-radius: 14px; background-image: linear-gradient(125deg, alpha(@accent_bg_color, 0.17), alpha(@accent_bg_color, 0.04)); border: 1px solid alpha(@accent_bg_color, 0.22); }
                 .packlift-package-icon { padding: 9px; border-radius: 12px; background-color: alpha(@card_bg_color, 0.8); }
                 .packlift-package-note { padding: 10px 12px; border-radius: 9px; background-color: @card_bg_color; }
                 .packlift-progress-window { background-color: @window_bg_color; border-radius: 12px; }
                 .packlift-progress { padding: 14px 16px; border-radius: 12px; background: @card_bg_color; }
                 .packlift-games-banner { padding: 20px; border-radius: 16px; color: white; background-image: linear-gradient(115deg, #6221aa, #063f78); }
                 .packlift-mobile-banner { padding: 20px; border-radius: 16px; color: white; background-image: linear-gradient(115deg, #123f8c, #70319b); }
                 .packlift-store-feature { padding: 20px 24px; border-radius: 18px; background-color: @card_bg_color; border: 1px solid alpha(@accent_bg_color, 0.23); }
                 .packlift-store-feature:hover, .packlift-store-feature:focus-visible { border-color: alpha(@accent_bg_color, 0.65); }
                 .packlift-store-preview { border-radius: 10px; box-shadow: 0 3px 12px alpha(black, 0.2); }
                 .packlift-store-skeleton-card { min-height: 82px; padding: 14px; border-radius: 12px; background: alpha(@window_fg_color, 0.045); border: 1px solid alpha(@borders, 0.42); }
                 .packlift-store-skeleton-icon { border-radius: 10px; background: alpha(@window_fg_color, 0.09); }
                 .packlift-store-skeleton-line { border-radius: 99px; background: alpha(@window_fg_color, 0.1); }
                 button.packlift-scope-option { padding: 8px 10px; border-radius: 10px; background: alpha(@window_fg_color, 0.07); }
                 button.packlift-scope-option:hover, button.packlift-scope-option:focus-visible { background: alpha(@accent_bg_color, 0.19); }
                 .packlift-scope-icon { padding: 7px; border-radius: 8px; color: @accent_color; background: alpha(@accent_bg_color, 0.15); }
                 .packlift-store-detail-hero { padding: 18px 20px; border-radius: 16px; background-color: @card_bg_color; border: 1px solid alpha(@borders, 0.5); }
                 .packlift-store-facts { border-radius: 12px; background: @borders; }
                 .packlift-store-facts > flowboxchild { padding: 0; background: @card_bg_color; }
                 .packlift-store-fact-content { min-height: 102px; padding: 16px 10px; }
                 .packlift-store-fact-badge { padding: 7px 10px; border-radius: 99px; color: @accent_color; background: alpha(@accent_bg_color, 0.14); }
                 .packlift-store-fact-button > button { padding: 0; background: transparent; border-radius: 0; }
                 .packlift-store-fact-button > button:hover, .packlift-store-fact-button > button:checked { background: alpha(@window_fg_color, 0.07); }
                 .packlift-store-shot { border-radius: 10px; }
                 .packlift-store-release { padding: 16px; }
                 .packlift-store-info-card { padding: 16px; border-radius: 12px; background: @card_bg_color; }
                 .packlift-review-stars { color: #bd7802; }
                 .packlift-store-card-icon { border-radius: 12px; background: alpha(@window_fg_color, 0.045); border: 1px solid alpha(@borders, 0.45); }
                 button.packlift-store-card { padding: 12px; border-radius: 12px; background: @card_bg_color; border: 1px solid alpha(@borders, 0.55); }
                 button.packlift-showcase-card { padding: 14px; border-radius: 15px; border-color: alpha(@borders, 0.55); }
                 .packlift-showcase-card .heading { font-size: 1.12em; }
                 .packlift-update-module { padding: 18px; border-radius: 16px; background: @card_bg_color; border: 1px solid alpha(@borders, 0.6); }
                 .packlift-update-preferences { padding: 14px; border-radius: 12px; background: alpha(@window_fg_color, 0.045); }
                 .packlift-update-section { padding: 18px; border-radius: 14px; background: @card_bg_color; border: 1px solid alpha(@borders, 0.55); }
                 .packlift-update-flatpak { padding: 18px; border-radius: 16px; background-image: linear-gradient(135deg, alpha(#4a90d9, 0.18), alpha(@card_bg_color, 0.98) 62%); border: 1px solid alpha(#4a90d9, 0.42); }
                 .packlift-flatpak-update-icon { padding: 3px; border-radius: 11px; }
                 .packlift-library-sidebar { padding: 6px; border-radius: 0 12px 12px 0; background: alpha(@window_fg_color, 0.035); }
                 .packlift-library-details { padding: 34px; }
                 .packlift-library-detail-hero { padding: 18px; border-radius: 14px; background-image: linear-gradient(120deg, alpha(@accent_bg_color, 0.16), alpha(@card_bg_color, 0.96) 70%); border: 1px solid alpha(@accent_bg_color, 0.24); }
                 .packlift-library-detail-row { padding: 12px 14px; border-radius: 9px; background: @card_bg_color; }
                 .packlift-update-hero { padding: 16px; border-radius: 14px; background-image: linear-gradient(115deg, alpha(@accent_bg_color, 0.24), alpha(@accent_bg_color, 0.08)); border: 1px solid alpha(@accent_bg_color, 0.34); }
                 .packlift-store-pagination { padding: 2px; border-radius: 99px; background: @card_bg_color; }
                 .packlift-store-pagination > button { min-width: 24px; min-height: 24px; padding: 0 4px; border-radius: 99px; }
                 .packlift-store-pagination > label { margin: 0 6px; }
                 button.packlift-store-category { padding: 12px; border: 0; border-radius: 12px; }
                 button.packlift-store-category:hover { border: 1px solid alpha(white, 0.42); }
                 box.packlift-store-category-icon-well { background: alpha(white, 0.18); border: 1px solid alpha(white, 0.20); border-radius: 12px; }
                 image.packlift-store-category-icon { color: inherit; }
                 button.packlift-store-category-create { color: white; background-image: linear-gradient(120deg, #a13eb2, #2858ae); }
                 button.packlift-store-category-work { color: #23364d; background-image: linear-gradient(120deg, #fff0b9, #f4d97c); }
                 button.packlift-store-category-play { color: white; background-image: linear-gradient(120deg, #f2a06f, #a832b8, #5333a3); }
                 button.packlift-store-category-socialize { color: white; background-image: linear-gradient(120deg, #dc2477, #e86b66); }
                 button.packlift-store-category-learn { color: white; background-image: linear-gradient(120deg, #29c778, #138a64); }
                 button.packlift-store-category-develop { color: white; background-image: linear-gradient(120deg, #55545f, #393840); }
                 .packlift-store-tint .dim-label { opacity: 0.87; }
                 button.packlift-store-card:hover { background: mix(@card_bg_color, @window_fg_color, 0.07); border-color: alpha(@window_fg_color, 0.28); }
                 button.packlift-store-card:focus-visible { border-color: @accent_color; }",
                &store_ui::app_color_styles(),
            ].concat());
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
    app.connect_startup(search_provider::register);
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
    window.set_default_size(1120, 760);
    if host_access_available() {
        store_ui::show(app, &window);
    } else {
        render_home(app, &window);
        window.present();
    }
}

fn render_home(app: &adw::Application, window: &adw::ApplicationWindow) {
    if window.has_css_class("packlift-package-window") {
        if let Some(parent) = window
            .transient_for()
            .and_downcast::<adw::ApplicationWindow>()
        {
            render_home(app, &parent);
            window.close();
            return;
        }
    }
    window.set_title(Some("Packlift"));
    window.set_widget_name("packlift-library");
    window.set_resizable(true);
    let host_access = host_access_available();
    if host_access {
        store_ui::preload(app, window);
    }
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

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(8);
    content.set_margin_bottom(0);
    content.set_margin_start(0);
    content.set_margin_end(0);

    let open = text_button(tr("Choose package…"));
    open.add_css_class("suggested-action");
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
                        open_package_review(&app, Some(&window), &path);
                    }
                }
            }
        });
    });
    header.pack_start(&open);

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
    let loading = library_status("system-search-symbolic", tr("Loading library…"), "");
    let spinner = gtk::Spinner::new();
    spinner.start();
    library.append(&loading);
    library.append(&spinner);
    let (owner, target, view) = (window.downgrade(), library.downgrade(), toolbar.downgrade());
    let application = app.clone();
    glib::MainContext::default().spawn_local(async move {
        let worker = gio::spawn_blocking(|| installer().and_then(|manager| manager.library_apps()));
        let records = worker.await.unwrap_or_else(|_| {
            Err(PackliftError::Message(tr("Could not load your library").into()))
        });
        let (Some(window), Some(library), Some(toolbar)) =
            (owner.upgrade(), target.upgrade(), view.upgrade())
        else { return; };
        while let Some(child) = library.first_child() {
            library.remove(&child);
        }
        populate_library(&application, &window, &toolbar, &library, records);
    });
    content.append(&library);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&content)
        .build();
    toolbar.set_content(Some(&scroll));
    updates_ui::attach(&toolbar, &header, window);
    window.set_content(Some(&toolbar));
    store.grab_focus();
}

fn populate_library(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    toolbar: &adw::ToolbarView,
    library: &gtk::Box,
    records: Result<Vec<Value>>,
) {
    match records {
        Ok(records) if records.is_empty() => {
            library.append(&home_label(tr("Your library"), "title-2"));
            library.append(&library_status(
                "folder-symbolic",
                tr("Your library is empty"),
                tr("Choose a package above to add your first program or game."),
            ));
        }
        Ok(mut records) => {
            records
                .sort_by_cached_key(|record| record["name"].as_str().unwrap_or("").to_lowercase());
            let search = gtk::SearchEntry::new();
            search.set_placeholder_text(Some(tr("Search programs and games")));
            search.set_hexpand(true);
            let search_focus = search.downgrade();
            page_shortcut(toolbar, "<Control>f", move || {
                if let Some(search) = search_focus.upgrade() {
                    search.grab_focus();
                }
            });
            search.update_property(&[gtk::accessible::Property::Label(tr(
                "Search programs and games",
            ))]);
            let filters = gtk::Box::new(gtk::Orientation::Vertical, 8);
            filters.append(&search);
            let labels: Vec<_> = LIBRARY_FILTERS.iter().map(|(label, _)| tr(label)).collect();
            let scope = gtk::DropDown::from_strings(&labels);
            scope.update_property(&[gtk::accessible::Property::Label(tr("Filter by app type"))]);
            filters.append(&scope);
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("boxed-list");
            let rows: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let row = library_row(&record);
                    let searchable = format!(
                        "{} {} {}",
                        row.title(),
                        row.subtitle().unwrap_or_default(),
                        record["target"].as_str().unwrap_or_default()
                    )
                    .to_lowercase();
                    row.set_activatable(true);
                    list.append(&row);
                    (row, searchable, record)
                })
                .collect();
            let rows = Rc::new(rows);
            let empty = library_status(
                "system-search-symbolic",
                tr("No matches"),
                tr("Try a different search or filter."),
            );
            empty.set_visible(false);
            let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 10);
            sidebar.set_size_request(290, -1);
            sidebar.add_css_class("packlift-library-sidebar");
            let sidebar_title = home_label(tr("Applications"), "title-3");
            sidebar.append(&sidebar_title);
            sidebar.append(&filters);
            let list_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .vexpand(true)
                .child(&list)
                .build();
            sidebar.append(&list_scroll);
            sidebar.append(&empty);

            let details = gtk::Box::new(gtk::Orientation::Vertical, 0);
            details.set_hexpand(true);
            details.set_vexpand(true);
            details.set_valign(gtk::Align::Start);
            details.add_css_class("packlift-library-details");
            let split = gtk::Paned::new(gtk::Orientation::Horizontal);
            split.set_start_child(Some(&sidebar));
            let detail_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&details)
                .build();
            split.set_end_child(Some(&detail_scroll));
            split.set_position(310);
            split.set_resize_start_child(false);
            split.set_shrink_start_child(false);
            split.set_vexpand(true);
            split.set_size_request(-1, 480);
            library.append(&split);

            let detail_target = details.downgrade();
            let detail_app = app.clone();
            let detail_window = window.downgrade();
            let selected_rows = rows.iter().map(|(_, _, record)| record.clone()).collect::<Vec<_>>();
            let scroll = detail_scroll.downgrade();
            list.connect_row_selected(move |_, selected| {
                let Some(details) = detail_target.upgrade() else { return; };
                while let Some(child) = details.first_child() {
                    details.remove(&child);
                }
                if let Some(scroll) = scroll.upgrade() {
                    scroll.vadjustment().set_value(0.0);
                }
                let Some(selected) = selected else {
                    return;
                };
                let index = selected.index();
                let Some(record) = usize::try_from(index)
                    .ok()
                    .and_then(|index| selected_rows.get(index))
                else {
                    return;
                };
                let Some(window) = detail_window.upgrade() else { return; };
                let detail = library_detail(record, &detail_app, &window);
                let clamp = adw::Clamp::builder()
                    .maximum_size(1000)
                    .child(&detail)
                    .build();
                details.append(&clamp);
            });
            if let Some((first, _, _)) = rows.first() {
                list.select_row(Some(first));
            }

            let list_ref = list.downgrade();
            let update_filter = Rc::new(move |query: &str, scope: u32| {
                let Some(list) = list_ref.upgrade() else { return; };
                let query = query.trim().to_lowercase();
                let filter = LIBRARY_FILTERS
                    .get(scope as usize)
                    .unwrap_or(&LIBRARY_FILTERS[0])
                    .1;
                let mut matches = false;
                for (row, searchable, record) in rows.iter() {
                    let visible = searchable.contains(&query) && filter(record);
                    row.set_visible(visible);
                    matches |= visible;
                }
                list.set_visible(matches);
                empty.set_visible(!matches);
                if !list.selected_row().is_some_and(|row| row.is_visible()) {
                    list.select_row(rows.iter().find(|(row, _, _)| row.is_visible()).map(|(row, _, _)| row.upcast_ref::<gtk::ListBoxRow>()));
                }
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
    if !class.is_empty() {
        label.add_css_class(class);
    }
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

fn library_row(record: &Value) -> adw::ActionRow {
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

    row
}

fn library_actions(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    record: &Value,
) -> gtk::Box {
    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let game = record["game_shortcut"].as_bool() == Some(true);
    let method = record["method"].as_str().unwrap_or("local");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
        launch.add_css_class("suggested-action");
        buttons.append(&launch);
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
    let uninstall = text_button(remove);
    uninstall.add_css_class("destructive-action");
    uninstall.set_valign(gtk::Align::Center);
    let name = name.to_owned();
    let record = record.clone();
    let app = app.clone();
    let parent = window.downgrade();
    uninstall.connect_clicked(move |_| {
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
                &format!("{} · {}", tr("Removing from your library"), record["name"].as_str().unwrap_or(tr("Program"))),
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
    buttons.append(&uninstall);
    buttons
}

fn library_detail(
    record: &Value,
    app: &adw::Application,
    window: &adw::ApplicationWindow,
) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 22);
    content.add_css_class("packlift-library-detail-content");
    content.set_hexpand(true);

    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let method = record["method"].as_str().unwrap_or("local");
    let game = record["game_shortcut"].as_bool() == Some(true);
    let description = match method {
        "flatpak" => flatpak_scope_label(record),
        "wine" if game => tr("Game shortcut").to_owned(),
        "wine" => tr("Windows program").to_owned(),
        "toolbox" => tr("Toolbx container").to_owned(),
        "native" => tr("Installed on this system").to_owned(),
        _ if record["sandbox_only"].as_bool() == Some(true) => {
            tr("Sandbox application").to_owned()
        }
        _ => tr("Local Linux application").to_owned(),
    };

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    hero.add_css_class("packlift-library-detail-hero");
    hero.set_valign(gtk::Align::Center);
    let icon = record_image(record, 92);
    icon.set_valign(gtk::Align::Center);
    hero.append(&icon);
    let identity = gtk::Box::new(gtk::Orientation::Vertical, 8);
    identity.set_valign(gtk::Align::Center);
    identity.set_hexpand(true);
    identity.append(&home_label(name, "title-1"));
    identity.append(&home_label(&description, "dim-label"));
    identity.append(&library_actions(app, window, record));
    hero.append(&identity);
    content.append(&hero);

    content.append(&home_label(tr("Details"), "title-3"));
    let fields = [
        (
            tr("Application ID"),
            record["flatpak_app_id"]
                .as_str()
                .or_else(|| record["id"].as_str()),
        ),
        (tr("Version"), record["version"].as_str()),
        (tr("Architecture"), record["architecture"].as_str()),
        (tr("Installed target"), record["target"].as_str()),
    ];
    for (label, value) in fields {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            continue;
        };
        content.append(&library_detail_row(label, value));
    }

    content.append(&home_label(tr("Permissions"), "title-3"));
    if method == "flatpak" {
        content.append(&flatpak_ui::editor(window, record));
    } else {
        let permissions = home_label(
            tr("Packlift does not manage permissions for this installation type."),
            "dim-label",
        );
        permissions.add_css_class("packlift-library-detail-row");
        content.append(&permissions);
    }

    content.append(&home_label(tr("Settings"), "title-3"));
    let settings = gtk::Box::new(gtk::Orientation::Vertical, 10);
    settings.add_css_class("packlift-library-detail-row");
    match method {
        "wine" => settings.append(&library_detail_row(
            tr("Runtime"),
            &program_runtime_label(record),
        )),
        "toolbox" => {
            let container = record["container"]
                .as_str()
                .unwrap_or(tr("Container"))
                .trim_start_matches("flatapk-");
            settings.append(&library_detail_row(tr("Container"), container));
        }
        "flatpak" => {
            settings.append(&home_label(
                tr("Installation scope and updates are handled by Flatpak."),
                "dim-label",
            ));
        }
        _ => {
            settings.append(&home_label(
                tr("This app has no additional settings in Packlift."),
                "dim-label",
            ));
        }
    }
    content.append(&settings);
    content
}

fn library_detail_row(label: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("packlift-library-detail-row");
    let key = home_label(label, "dim-label");
    key.set_size_request(130, -1);
    key.set_valign(gtk::Align::Start);
    let value = home_label(value, "");
    value.set_selectable(true);
    value.set_hexpand(true);
    row.append(&key);
    row.append(&value);
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

fn open_package(app: &adw::Application, path: &Path) {
    let parent = app
        .active_window()
        .and_downcast::<adw::ApplicationWindow>()
        .filter(|window| {
            window.widget_name() == "packlift-library" || window.widget_name() == "packlift-store"
        })
        .filter(|window| !window.has_css_class("packlift-running-job"));
    open_package_review(app, parent.as_ref(), path);
}

fn open_package_review(
    app: &adw::Application,
    parent: Option<&adw::ApplicationWindow>,
    path: &Path,
) {
    let window = adw::ApplicationWindow::new(app);
    window.set_default_size(470, -1);
    if let Some(parent) = parent {
        window.add_css_class("packlift-package-window");
        window.set_transient_for(Some(parent));
        parent.set_visible(false);
        window.connect_close_request(|window| {
            if let Some(parent) = window.transient_for() {
                parent.present();
            }
            glib::Propagation::Proceed
        });
    }
    review_package(app, &window, path);
}

fn review_package(app: &adw::Application, window: &adw::ApplicationWindow, path: &Path) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Review package"), "")));
    if window.transient_for().is_some() {
        let review = window.downgrade();
        page_navigation(
            &toolbar,
            &header,
            tr("Back"),
            "go-previous-symbolic",
            move || {
                if let Some(window) = review.upgrade() {
                    window.close();
                }
            },
        );
    }
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
            let progress = gtk::Box::new(gtk::Orientation::Vertical, 7);
            progress.add_css_class("packlift-progress");
            progress.set_visible(false);
            progress.set_margin_start(16);
            progress.set_margin_end(16);
            progress.set_margin_top(10);
            toolbar.add_bottom_bar(&progress);
            let install_button = install.downgrade();
            progress.connect_visible_notify(move |progress| {
                if let Some(install) = install_button.upgrade() {
                    install.set_sensitive(!progress.is_visible());
                }
            });
            let progress = progress.downgrade();
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
                } else if let (Some(window), Some(progress)) =
                    (weak_window.upgrade(), progress.upgrade())
                {
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
                        let title = format!("{} · {}", tr("Installing program or game"), package.name);
                        run_job_with_progress(
                            &app,
                            &window,
                            &title,
                            Some(&progress),
                            move |logger| {
                                installer_with_output(logger)?
                                    .install_windows(&package, &runtime)
                                    .map(Some)
                            },
                        );
                    } else {
                        let title = format!("{} · {}", tr("Installing program"), package.name);
                        run_job_with_progress(&app, &window, &title, Some(&progress), move |logger| {
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
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
    scroll.set_propagate_natural_height(true);
    scroll.set_max_content_height(480);
    scroll.set_child(Some(&body));
    toolbar.set_content(Some(&scroll));
    operations_ui::attach_button(&header);
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
    operations_ui::attach_button(&header);
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
    run_job_with_progress(app, window, title, None, task);
}

fn run_job_with_progress<F>(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    title: &str,
    inline: Option<&gtk::Box>,
    task: F,
) where
    F: FnOnce(LogHandler) -> Result<Option<Value>> + Send + 'static,
{
    let library = window.widget_name() == "packlift-library";
    let package_review = window.widget_name() == "packlift-package-review";
    if operations_ui::has_active_title(title)
        || inline.map_or_else(
            || !library && window.has_css_class("packlift-running-job"),
            gtk::prelude::WidgetExt::is_visible,
        )
    {
        return;
    }
    let is_inline = inline.is_some();
    if let Some(slot) = inline {
        if !package_review {
            slot.append(&home_label(tr("In the operations queue"), "caption"));
        }
        slot.set_visible(true);
    } else if !library {
        window.add_css_class("packlift-running-job");
    }

    let progress_slot = inline.map(ObjectExt::downgrade);
    let release_slot = progress_slot.clone();
    let owner = window.downgrade();
    let original = window.content().map(|content| content.downgrade());
    let application = app.clone();
    let completed_owner = owner.clone();
    let id = operations_ui::submit(app.upcast_ref(), title, task, move |result| {
        let window = completed_owner.upgrade().filter(|window| window.application().is_some());
        let still_here = window.as_ref().is_some_and(|window| {
            original.as_ref().and_then(glib::WeakRef::upgrade) == window.content()
        });
        if let Ok(Some(record)) = &result {
            if record["method"] == "flatpak"
                && record["flatpak_already_installed"] != true
                && !(package_review && still_here)
            {
                notify_flatpak_installed(&application, record);
            }
        }
        let Some(window) = window else { return; };
        match result {
            Err(PackliftError::InstalledExecutableRequired { package, runtime }) => {
                choose_installed_executable(&application, &window, *package, runtime);
            }
            Err(PackliftError::ExecutableSelectionFailed { package, runtime, reason }) => {
                choose_installed_executable(&application, &window, *package, runtime);
                show_error(&window, &reason);
            }
            Err(error) if (package_review || !is_inline) && still_here => show_error(&window, &error.to_string()),
            Ok(Some(record)) if still_here && (package_review || record["method"] != "flatpak") => {
                show_success(&application, &window, &record);
            }
            Ok(_) => {
                if window.widget_name() == "packlift-library"
                    || (still_here && window.has_css_class("packlift-package-window"))
                {
                    render_home(&application, &window);
                }
            }
            Err(_) => {}
        }
    }, move || {
        if let Some(window) = owner.upgrade() {
            if !is_inline && !library {
                window.remove_css_class("packlift-running-job");
            }
        }
        if let Some(slot) = release_slot.and_then(|slot| slot.upgrade()) {
            while let Some(child) = slot.first_child() {
                slot.remove(&child);
            }
            slot.set_visible(false);
        }
    });
    if let Some(slot) = progress_slot.and_then(|slot| slot.upgrade()).filter(|_| package_review) {
        operations_ui::track(&slot, id, title);
    }
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
    operations_ui::attach_button(&header);
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
    let package_review = window.widget_name() == "packlift-package-review";
    let already_installed = record["flatpak_already_installed"] == true;
    let self_install = record["flatpak_app_id"] == APP_ID;
    let record_is_flatpak = record["method"] == "flatpak";
    let target = window.downgrade();
    let finish = Rc::new(move || {
        if let Some(window) = target.upgrade() {
            if package_review && record_is_flatpak {
                window.close();
            } else {
                render_home(&app, &window);
            }
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
    } else if already_installed {
        tr("Already installed")
    } else {
        tr("Installation complete")
    }));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Center);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body.append(&title);
    let name = record["name"].as_str().unwrap_or(tr("Program"));
    let runnable = record["exports"][0]["argv"].is_array();
    let message = if already_installed {
        tr("This version of {name} is already installed.").replace("{name}", name)
    } else if self_install {
        tr("Close all Packlift windows and open it again from your app menu to use this version.").to_owned()
    } else if runnable {
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
    if runnable && !self_install {
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
        .vscrollbar_policy(gtk::PolicyType::External)
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
        .vscrollbar_policy(gtk::PolicyType::External)
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
        .vscrollbar_policy(gtk::PolicyType::External)
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
