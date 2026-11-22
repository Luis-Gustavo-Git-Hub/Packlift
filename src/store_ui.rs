use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use packlift::{
    store::{self, CatalogApp, CatalogIcon, CatalogSearch, Destination, Remote, Screenshot},
    system::Runner,
};
use serde_json::{json, Value};

use crate::{
    back_to_previous, home_label, i18n::tr, page_navigation, page_shortcut, render_home, run_job,
    show_host_access_dialog, text_button,
};

struct StoreView {
    window: glib::WeakRef<adw::ApplicationWindow>,
    search: gtk::SearchEntry,
    submit: gtk::Button,
    feature: gtk::Box,
    list: gtk::FlowBox,
    status: gtk::Label,
    scroll: gtk::ScrolledWindow,
    previous: gtk::Button,
    next: gtk::Button,
    page_label: gtk::Label,
    remotes: RefCell<Vec<Remote>>,
    catalog_search: RefCell<Option<CatalogSearch>>,
    page: Cell<usize>,
    busy: Cell<bool>,
    pending_search: Cell<bool>,
}

pub(super) fn show(app: &adw::Application, parent: &adw::ApplicationWindow) {
    if !packlift::system::host_access_available() {
        show_host_access_dialog(parent);
        return;
    }
    let window = parent;
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Store"), "")));
    let home = window.downgrade();
    let application = app.clone();
    page_navigation(
        &toolbar,
        &header,
        tr("Library"),
        "view-grid-symbolic",
        move || {
            if let Some(window) = home.upgrade() {
                render_home(&application, &window);
            }
        },
    );
    toolbar.add_top_bar(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    for (orientation, margin) in [
        (gtk::Orientation::Horizontal, 20),
        (gtk::Orientation::Vertical, 16),
    ] {
        if orientation == gtk::Orientation::Horizontal {
            body.set_margin_start(margin);
            body.set_margin_end(margin);
        } else {
            body.set_margin_top(margin);
            body.set_margin_bottom(margin);
        }
    }
    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr("Search the store")));
    search.update_property(&[gtk::accessible::Property::Label(tr("Search the store"))]);
    search.set_hexpand(true);
    let submit = text_button(tr("Search"));
    submit.add_css_class("suggested-action");
    search_row.append(&search);
    search_row.append(&submit);
    body.append(&search_row);

    let status = home_label("", "dim-label");
    status.set_selectable(true);
    body.append(&status);
    let feature = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let list = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .homogeneous(true)
        .row_spacing(10)
        .column_spacing(10)
        .build();
    let catalog = gtk::Box::new(gtk::Orientation::Vertical, 14);
    catalog.append(&feature);
    catalog.append(&list);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&catalog)
        .build();
    body.append(&scroll);
    let nav = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    nav.set_halign(gtk::Align::Center);
    let previous = text_button(tr("Previous"));
    let next = text_button(tr("Next"));
    let page_label = home_label("", "dim-label");
    nav.append(&previous);
    nav.append(&page_label);
    nav.append(&next);
    body.append(&nav);
    let layout = adw::Clamp::builder()
        .maximum_size(1180)
        .tightening_threshold(1000)
        .child(&body)
        .build();
    toolbar.set_content(Some(&layout));
    window.set_widget_name("packlift-store");
    window.set_title(Some(tr("Store")));
    window.set_content(Some(&toolbar));
    let search_target = search.downgrade();
    page_shortcut(&toolbar, "<Control>f", move || {
        if let Some(search) = search_target.upgrade() {
            search.grab_focus();
        }
    });

    let view = Rc::new(StoreView {
        window: window.downgrade(),
        search,
        submit,
        feature,
        list,
        status,
        scroll,
        previous,
        next,
        page_label,
        remotes: RefCell::new(Vec::new()),
        catalog_search: RefCell::new(None),
        page: Cell::new(1),
        busy: Cell::new(false),
        pending_search: Cell::new(false),
    });
    let weak = Rc::downgrade(&view);
    view.search.connect_activate(move |_| {
        if let Some(view) = weak.upgrade() {
            if view.busy.get() {
                view.pending_search.set(true);
            } else {
                view.new_search();
            }
        }
    });
    let weak = Rc::downgrade(&view);
    view.submit.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.new_search();
        }
    });
    let weak = Rc::downgrade(&view);
    view.previous.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.load_page(view.page.get().saturating_sub(1).max(1));
        }
    });
    let weak = Rc::downgrade(&view);
    view.next.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.load_page(view.page.get() + 1);
        }
    });
    let keep_alive = view.clone();
    toolbar.connect_destroy(move |_| {
        let _ = &keep_alive;
    });
    view.load_remotes();
    window.present();
    view.search.grab_focus();
}

impl StoreView {
    fn set_status(&self, message: &str) {
        self.status.set_text(message);
        self.status.set_visible(!message.is_empty());
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.submit
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
        if busy {
            self.previous.set_sensitive(false);
            self.next.set_sensitive(false);
        }
    }

    fn load_remotes(self: &Rc<Self>) {
        self.set_busy(true);
        self.set_status(tr("Loading repositories…"));
        let view = self.clone();
        background(
            move || worker().and_then(|runner| store::catalog_remotes(&runner)),
            move |result| match result {
                Ok(remotes) => {
                    *view.remotes.borrow_mut() = remotes;
                    view.new_search();
                }
                Err(error) => {
                    view.set_status(&error.to_string());
                    view.set_busy(false);
                }
            },
        );
    }

    fn new_search(self: &Rc<Self>) {
        self.pending_search.set(false);
        let query = self.search.text().trim().to_owned();
        *self.catalog_search.borrow_mut() =
            Some(CatalogSearch::new(self.remotes.borrow().clone(), query));
        self.load_page(1);
    }

    fn load_page(self: &Rc<Self>, page: usize) {
        let Some(mut catalog_search) = self.catalog_search.borrow_mut().take() else {
            return;
        };
        self.set_busy(true);
        self.list.remove_all();
        if let Some(child) = self.feature.first_child() {
            self.feature.remove(&child);
        }
        self.page_label.set_text("");
        self.set_status(tr("Searching the store…"));
        let explore = catalog_search.is_explore() && page == 1;
        let view = self.clone();
        background(
            move || {
                let result = worker().and_then(|runner| catalog_search.page(&runner, page));
                (catalog_search, result)
            },
            move |(catalog_search, result)| {
                *view.catalog_search.borrow_mut() = Some(catalog_search);
                if view.pending_search.get() {
                    view.new_search();
                    return;
                }
                view.set_busy(false);
                match result {
                    Ok(results) => {
                        view.page.set(page);
                        let adjustment = view.scroll.vadjustment();
                        adjustment.set_value(adjustment.lower());
                        view.previous.set_sensitive(page > 1);
                        view.next.set_sensitive(results.has_next);
                        view.page_label
                            .set_text(&tr("Page {number}").replace("{number}", &page.to_string()));
                        view.set_status(if results.apps.is_empty() {
                            tr("No apps match this search.")
                        } else {
                            ""
                        });
                        let mut apps = results.apps.into_iter();
                        if explore {
                            if let Some((remote, app)) = apps.next() {
                                view.feature
                                    .append(&featured_app(&view.window, remote, app));
                            }
                        }
                        for (remote, app) in apps {
                            view.list.append(&app_card(&view.window, remote, app));
                        }
                    }
                    Err(error) => {
                        view.page.set(page);
                        view.previous.set_sensitive(page > 1);
                        view.set_status(&error.to_string());
                    }
                }
            },
        );
    }
}

fn show_app(window: &adw::ApplicationWindow, remote: &Remote, app: &CatalogApp) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Store"), "")));
    back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 24);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(18);
    body.set_margin_bottom(24);

    let hero = gtk::Box::new(gtk::Orientation::Vertical, 16);
    hero.add_css_class("packlift-store-detail-hero");
    let identity = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let icon = catalog_icon(app, 80);
    icon.set_valign(gtk::Align::Center);
    identity.append(&icon);
    let title = gtk::Box::new(gtk::Orientation::Vertical, 4);
    title.set_hexpand(true);
    title.set_valign(gtk::Align::Center);
    title.append(&home_label(&app.name, "title-1"));
    let developer = home_label("", "dim-label");
    developer.set_visible(false);
    title.append(&developer);
    identity.append(&title);
    let install = gtk::Button::with_label(tr("Install"));
    install.set_valign(gtk::Align::Center);
    install.add_css_class("suggested-action");
    install.add_css_class("pill");
    install.set_width_request(104);
    install.update_property(&[gtk::accessible::Property::Label(tr("Install"))]);
    identity.append(&install);
    hero.append(&identity);

    let facts = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(6)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    facts.set_visible(false);
    hero.append(&facts);
    let permissions = adw::ExpanderRow::new();
    permissions.set_title(tr("Permissions"));
    permissions.set_visible(false);
    let permissions_group = gtk::ListBox::new();
    permissions_group.set_selection_mode(gtk::SelectionMode::None);
    permissions_group.add_css_class("boxed-list");
    permissions_group.append(&permissions);
    permissions_group.set_visible(false);
    hero.append(&permissions_group);
    body.append(&hero);

    let install_options = gtk::Box::new(gtk::Orientation::Vertical, 0);
    install_options.set_visible(false);
    body.append(&install_options);

    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let gallery_status = home_label(tr("Loading screenshots…"), "dim-label");
    gallery.append(&gallery_status);
    body.append(&gallery);

    let about = gtk::Box::new(gtk::Orientation::Vertical, 8);
    about.append(&home_label(tr("About this app"), "title-3"));
    if !app.summary.is_empty() {
        about.append(&home_label(&app.summary, "heading"));
    }
    let description = home_label("", "");
    description.set_selectable(true);
    description.set_visible(false);
    about.append(&description);
    let app_id = home_label(&app.id, "dim-label");
    app_id.set_selectable(true);
    about.append(&app_id);
    body.append(&about);

    let releases = gtk::Box::new(gtk::Orientation::Vertical, 12);
    releases.set_visible(false);
    body.append(&releases);
    let (sources_section, sources) = detail_list(tr("Sources"));
    body.append(&sources_section);
    let (links_section, links) = detail_list(tr("Links"));
    body.append(&links_section);
    let similar_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    similar_section.set_visible(false);
    similar_section.append(&home_label(tr("Similar apps"), "title-3"));
    let similar = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .homogeneous(true)
        .row_spacing(8)
        .column_spacing(8)
        .build();
    similar_section.append(&similar);
    body.append(&similar_section);

    let layout = adw::Clamp::builder()
        .maximum_size(1120)
        .tightening_threshold(940)
        .child(&body)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&layout)
        .build();
    toolbar.set_content(Some(&scroll));
    window.set_widget_name("packlift-store-details");
    window.set_title(Some(&app.name));
    let installed = Rc::new(RefCell::new(Vec::new()));
    let installed_state = installed.clone();
    let refresh_button = install.downgrade();
    let app_id = app.id.clone();
    toolbar.connect_map(move |_| {
        let Some(button) = refresh_button.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        let id = app_id.clone();
        let state = installed_state.clone();
        let refresh_button = refresh_button.clone();
        background(
            move || crate::installer()?.flatpak_app_installations(&id),
            move |result| {
                let Some(button) = refresh_button.upgrade() else {
                    return;
                };
                match result {
                    Ok(records) => {
                        let is_installed = !records.is_empty();
                        *state.borrow_mut() = records;
                        let label = if is_installed {
                            tr("Uninstall")
                        } else {
                            tr("Install")
                        };
                        button.set_label(label);
                        button.update_property(&[gtk::accessible::Property::Label(label)]);
                        button.set_css_classes(&[
                            "pill",
                            if is_installed {
                                "destructive-action"
                            } else {
                                "suggested-action"
                            },
                        ]);
                        button.set_sensitive(true);
                        if button.has_visible_focus()
                            || button.root().is_some_and(|root| root.focus().is_none())
                        {
                            button.grab_focus();
                        }
                    }
                    Err(error) => button.set_tooltip_text(Some(&error.to_string())),
                }
            },
        );
    });
    let selected = remote.clone();
    let package = app.clone();
    let dialog = window.downgrade();
    let install_options = install_options.downgrade();
    install.connect_clicked(move |button| {
        let Some(dialog) = dialog.upgrade() else {
            return;
        };
        let records = installed.borrow().clone();
        if !records.is_empty() {
            if let Some(options) = install_options.upgrade() {
                choose_uninstall_destination(&dialog, &options, button, records, &package.name);
            }
            return;
        }
        let Some(current_page) = dialog.content() else {
            return;
        };
        let current_page = current_page.downgrade();
        button.set_sensitive(false);
        let spinner = gtk::Spinner::new();
        spinner.start();
        button.set_child(Some(&spinner));
        button.announce(
            tr("Loading repositories…"),
            gtk::AccessibleAnnouncementPriority::Low,
        );
        let source = selected.clone();
        let candidate = package.clone();
        let package_name = candidate.name.clone();
        let button = button.downgrade();
        let dialog = dialog.downgrade();
        let preferred = source.destination;
        let options = install_options.clone();
        background(
            move || {
                let Ok(runner) = worker() else {
                    return (Vec::new(), Vec::new());
                };
                std::thread::scope(|tasks| {
                    let user = tasks.spawn(|| {
                        store::install_choices(&runner, &source, &candidate, Destination::User)
                    });
                    let system =
                        store::install_choices(&runner, &source, &candidate, Destination::System);
                    (user.join().unwrap_or_default(), system)
                })
            },
            move |choices| {
                let Some(button) = button.upgrade() else {
                    return;
                };
                button.set_sensitive(true);
                button.set_label(tr("Install"));
                let (Some(dialog), Some(options), Some(page)) =
                    (dialog.upgrade(), options.upgrade(), current_page.upgrade())
                else {
                    return;
                };
                if dialog.content().as_ref() == Some(&page) {
                    choose_install_destination(
                        &dialog,
                        &options,
                        &button,
                        preferred,
                        choices,
                        &package_name,
                    );
                }
            },
        );
    });

    load_detail_sources(remote, app, sources_section, sources);
    if remote.flathub {
        let selected = app.clone();
        let indicators = facts.clone();
        background(
            move || {
                worker()
                    .ok()
                    .map(|runner| store::flathub_facts(&runner, &selected))
            },
            move |result| {
                let Some(result) = result else {
                    return;
                };
                if let Some(size) = result.download_size {
                    indicators.insert(
                        &store_fact(tr("Download size"), &glib::format_size(size)),
                        0,
                    );
                    indicators.set_visible(true);
                }
                if let Some(monthly) = result.installs_last_month {
                    indicators.append(&store_fact(tr("Installs last month"), &monthly.to_string()));
                    indicators.set_visible(true);
                }
                if let Some(groups) = result.permissions {
                    if groups.is_empty() {
                        permissions.set_subtitle(tr("No permissions listed by the repository"));
                    }
                    for (label, access) in groups {
                        permissions.add_row(&detail_row(tr(label), &access));
                    }
                    permissions.set_visible(true);
                    permissions_group.set_visible(true);
                }
            },
        );
    }
    let source = remote.clone();
    let catalog_source = remote.clone();
    let selected = app.clone();
    let id = app.id.clone();
    let parent = window.downgrade();
    background(
        move || worker().and_then(|runner| store::app_details(&runner, &source, &selected)),
        move |result| match result {
            Ok(details) => {
                developer.set_text(&details.developer);
                developer.set_visible(!details.developer.is_empty());
                if !details.license.is_empty() {
                    let value = match details.free_software {
                        Some(true) => tr("Free software"),
                        Some(false) => tr("Proprietary software"),
                        None => &details.license,
                    };
                    let license = store_fact(tr("License"), value);
                    license.set_tooltip_text(Some(&details.license));
                    facts.append(&license);
                    facts.set_visible(true);
                }
                if let Some(adaptive) = details.adaptive {
                    facts.append(&store_fact(
                        tr("Device support"),
                        if adaptive {
                            tr("Adaptive")
                        } else {
                            tr("Desktop")
                        },
                    ));
                    facts.set_visible(true);
                }
                if let Some(age) = details.age {
                    facts.append(&store_fact(tr("Age rating"), &format!("{age}+")));
                    facts.set_visible(true);
                }
                if !details.description.is_empty() {
                    description.set_text(&details.description);
                    description.set_visible(true);
                }
                show_release_history(&releases, details.releases);
                show_detail_links(&links_section, &links, &parent, details.links);
                if let Some(category) = details.category {
                    background(
                        move || {
                            worker().and_then(|runner| store::similar_apps(&runner, &id, &category))
                        },
                        move |result| {
                            if let Ok(apps) = result {
                                similar_section.set_visible(!apps.is_empty());
                                for app in apps {
                                    similar.append(&app_card(&parent, catalog_source.clone(), app));
                                }
                            }
                        },
                    );
                }
                if details.screenshots.is_empty() {
                    gallery_status
                        .set_text(tr("No screenshots are available from this repository."));
                } else {
                    gallery.remove(&gallery_status);
                    bind_gallery(&gallery, details.screenshots);
                }
            }
            Err(_) => gallery_status.set_text(tr("Screenshots could not be loaded.")),
        },
    );
    window.set_content(Some(&toolbar));
    install.grab_focus();
    window.present();
}

fn detail_list(title: &str) -> (gtk::Box, gtk::ListBox) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.set_visible(false);
    section.append(&home_label(title, "title-3"));
    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.add_css_class("boxed-list");
    section.append(&rows);
    (section, rows)
}

fn detail_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_use_markup(false);
    row.set_title(title);
    row.set_subtitle(value);
    row.set_subtitle_lines(2);
    row
}

fn show_release_history(container: &gtk::Box, releases: Vec<store::Release>) {
    let mut releases = releases.into_iter();
    let Some(latest) = releases.next() else {
        return;
    };
    container.append(&release_card(latest));
    let older = releases.collect::<Vec<_>>();
    if !older.is_empty() {
        let history = adw::ExpanderRow::new();
        history.set_title(tr("Version history"));
        for release in older {
            history.add_row(&release_card(release));
        }
        let group = gtk::ListBox::new();
        group.set_selection_mode(gtk::SelectionMode::None);
        group.add_css_class("boxed-list");
        group.append(&history);
        container.append(&group);
    }
    container.set_visible(true);
}

fn release_card(release: store::Release) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("packlift-store-release");
    let version = tr("Version {version}").replace("{version}", &release.version);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = home_label(&version, "title-3");
    label.set_hexpand(true);
    header.append(&label);
    let date = release.date.or_else(|| {
        let date = glib::DateTime::from_unix_utc(release.timestamp?).ok()?;
        date.format("%x").ok().map(|value| value.to_string())
    });
    if let Some(date) = date {
        header.append(&home_label(&date, "dim-label"));
    }
    card.append(&header);
    if !release.notes.is_empty() {
        let notes = home_label(&release.notes, "");
        notes.set_selectable(true);
        card.append(&notes);
    }
    card
}

fn show_detail_links(
    section: &gtk::Box,
    rows: &gtk::ListBox,
    parent: &glib::WeakRef<adw::ApplicationWindow>,
    links: Vec<(&'static str, String)>,
) {
    section.set_visible(!links.is_empty());
    for (name, url) in links {
        let row = detail_row(tr(name), &url);
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        let open = gtk::Button::from_icon_name("go-jump-symbolic");
        for (button, label) in [(&copy, tr("Copy link")), (&open, tr("Open link"))] {
            button.set_valign(gtk::Align::Center);
            button.set_tooltip_text(Some(label));
            button.update_property(&[gtk::accessible::Property::Label(label)]);
            row.add_suffix(button);
        }
        let link = url.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&link);
            button.announce(tr("Link copied"), gtk::AccessibleAnnouncementPriority::Low);
        });
        let parent = parent.clone();
        open.connect_clicked(move |_| {
            if let Some(window) = parent.upgrade() {
                let parent = window.downgrade();
                gtk::UriLauncher::new(&url).launch(
                    Some(&window),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let (Some(window), Err(error)) = (parent.upgrade(), result) {
                            crate::show_error(&window, &error.to_string());
                        }
                    },
                );
            }
        });
        rows.append(&row);
    }
}

fn load_detail_sources(remote: &Remote, app: &CatalogApp, section: gtk::Box, rows: gtk::ListBox) {
    let source = remote.clone();
    let candidate = app.clone();
    background(
        move || {
            worker().map(|runner| {
                std::thread::scope(|tasks| {
                    let user = tasks.spawn(|| {
                        store::install_choices(&runner, &source, &candidate, Destination::User)
                    });
                    let system =
                        store::install_choices(&runner, &source, &candidate, Destination::System);
                    user.join()
                        .unwrap_or_default()
                        .into_iter()
                        .chain(system)
                        .collect::<Vec<_>>()
                })
            })
        },
        move |result| {
            let Ok(choices) = result else {
                return;
            };
            section.set_visible(!choices.is_empty());
            for (remote, _) in choices {
                let row = detail_row(&remote.label(), &remote.url);
                let scope = match remote.destination {
                    Destination::User => tr("For my account"),
                    Destination::System => tr("For the system"),
                };
                let scope = gtk::Label::new(Some(scope));
                scope.add_css_class("dim-label");
                scope.set_valign(gtk::Align::Center);
                row.add_suffix(&scope);
                rows.append(&row);
            }
        },
    );
}

fn store_fact(label: &str, value: &str) -> gtk::Box {
    let fact = gtk::Box::new(gtk::Orientation::Vertical, 3);
    fact.add_css_class("packlift-store-fact");
    fact.append(&home_label(label, "dim-label"));
    fact.append(&home_label(value, "heading"));
    fact
}

fn app_card(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: Remote,
    app: CatalogApp,
) -> gtk::Button {
    let button = gtk::Button::builder().hexpand(true).build();
    button.add_css_class("packlift-store-card");
    button.set_size_request(190, 76);
    button.update_property(&[gtk::accessible::Property::Label(&app.name)]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_valign(gtk::Align::Center);
    content.append(&catalog_icon(&app, 46));
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let name = home_label(&app.name, "heading");
    name.set_lines(1);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(22);
    let summary = home_label(
        if app.summary.is_empty() {
            &app.id
        } else {
            &app.summary
        },
        "dim-label",
    );
    summary.set_lines(2);
    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.set_max_width_chars(25);
    text.append(&name);
    text.append(&summary);
    content.append(&text);
    button.set_child(Some(&content));
    let window = window.clone();
    button.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            show_app(&window, &remote, &app);
        }
    });
    button
}

fn featured_app(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: Remote,
    app: CatalogApp,
) -> gtk::Box {
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    hero.add_css_class("packlift-store-feature");
    let intro = gtk::Box::new(gtk::Orientation::Vertical, 8);
    intro.set_hexpand(true);
    intro.set_halign(gtk::Align::Center);
    intro.set_valign(gtk::Align::Center);
    let icon = catalog_icon(&app, 56);
    icon.set_halign(gtk::Align::Start);
    intro.append(&icon);
    let name = home_label(&app.name, "title-1");
    name.set_max_width_chars(24);
    intro.append(&name);
    let summary = home_label(&app.summary, "dim-label");
    summary.set_max_width_chars(30);
    if !app.summary.is_empty() {
        intro.append(&summary);
    }
    let details = text_button(tr("View app"));
    details.set_halign(gtk::Align::Start);
    details.set_margin_top(4);
    details.add_css_class("suggested-action");
    intro.append(&details);
    hero.append(&intro);

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .height_request(208)
        .hexpand(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    picture.add_css_class("packlift-store-preview");
    picture.set_overflow(gtk::Overflow::Hidden);
    picture.set_visible(false);
    hero.append(&picture);

    let selected = app.clone();
    let origin = remote.clone();
    let image = picture.downgrade();
    self::background(
        move || {
            let runner = worker().ok()?;
            let shot = store::app_details(&runner, &origin, &selected)
                .ok()?
                .screenshots
                .into_iter()
                .next()?;
            let bytes = store::screenshot_bytes(&runner, &shot.url).ok()?;
            Some((bytes, shot.caption))
        },
        move |result| {
            let (Some((bytes, caption)), Some(picture)) = (result, image.upgrade()) else {
                return;
            };
            let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
            if let Ok(preview) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                &stream,
                520,
                244,
                true,
                gio::Cancellable::NONE,
            ) {
                let texture = gtk::gdk::Texture::for_pixbuf(&preview);
                picture.set_paintable(Some(&texture));
                if !caption.is_empty() {
                    picture.set_alternative_text(Some(&caption));
                }
                picture.set_visible(true);
            }
        },
    );
    let parent = window.clone();
    details.connect_clicked(move |_| {
        if let Some(window) = parent.upgrade() {
            show_app(&window, &remote, &app);
        }
    });
    hero
}

type IconKey = (CatalogIcon, String);
type IconTarget = (glib::WeakRef<gtk::Image>, i32);

#[derive(Default)]
struct IconQueue {
    ready: HashMap<IconKey, gtk::gdk_pixbuf::Pixbuf>,
    pending: HashMap<IconKey, Vec<IconTarget>>,
    queue: VecDeque<IconKey>,
    active: usize,
    scheduled: bool,
}

thread_local! {
    static ICONS: RefCell<IconQueue> = RefCell::new(IconQueue::default());
}

fn catalog_icon(app: &CatalogApp, size: i32) -> gtk::Image {
    let has_icon = gtk::gdk::Display::default()
        .is_some_and(|display| gtk::IconTheme::for_display(&display).has_icon(&app.id));
    let fallback = if has_icon {
        &app.id
    } else {
        "application-x-executable-symbolic"
    };
    let image = gtk::Image::from_icon_name(fallback);
    image.set_pixel_size(size);
    image.set_size_request(size, size);
    if let Some(source) = app.icon.clone().filter(|_| !has_icon) {
        let key = (source, app.id.clone());
        // Cache hits must be painted before mapping so the fallback does not flash.
        let ready = ICONS.with_borrow(|icons| icons.ready.get(&key).cloned());
        if let Some(pixbuf) = ready {
            paint_icon(&image, &pixbuf, size);
        } else {
            image.connect_map(move |image| queue_icon(image, size, &key));
        }
    }
    image
}

fn queue_icon(image: &gtk::Image, size: i32, key: &IconKey) {
    let ready = ICONS.with_borrow_mut(|icons| {
        if let Some(pixbuf) = icons.ready.get(key) {
            return Some(pixbuf.clone());
        }
        match icons.pending.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .retain(|(target, _)| target.upgrade().is_some_and(|target| target != *image));
                entry.get_mut().push((image.downgrade(), size));
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(vec![(image.downgrade(), size)]);
                icons.queue.push_back(key.clone());
            }
        }
        if !icons.scheduled {
            icons.scheduled = true;
            glib::idle_add_local_once(load_queued_icons);
        }
        None
    });
    if let Some(pixbuf) = ready {
        paint_icon(image, &pixbuf, size);
    }
}

fn load_queued_icons() {
    const MAX_ACTIVE: usize = 6;
    let jobs = ICONS.with_borrow_mut(|icons| {
        icons.scheduled = false;
        let mut jobs = Vec::new();
        while icons.active < MAX_ACTIVE && !icons.queue.is_empty() {
            let visible = icons.queue.iter().position(|key| {
                icons.pending[key]
                    .iter()
                    .filter_map(|(image, _)| image.upgrade())
                    .any(|image| {
                        if !image.is_mapped() {
                            return false;
                        }
                        if let Some(scroll) = image.ancestor(gtk::ScrolledWindow::static_type()) {
                            image.compute_bounds(&scroll).is_some_and(|bounds| {
                                bounds.y() < scroll.height() as f32
                                    && bounds.y() + bounds.height() > 0.0
                            })
                        } else {
                            true
                        }
                    })
            });
            let key = icons.queue.remove(visible.unwrap_or(0)).unwrap();
            if icons.pending[&key]
                .iter()
                .all(|(image, _)| image.upgrade().is_none())
            {
                icons.pending.remove(&key);
                continue;
            }
            icons.active += 1;
            jobs.push(key);
        }
        jobs
    });
    for key in jobs {
        let (source, id) = key.clone();
        background(
            move || {
                worker()
                    .ok()
                    .and_then(|runner| store::catalog_icon_bytes(&runner, &source, &id))
            },
            move |bytes| {
                let pixbuf = bytes.and_then(|bytes| {
                    let stream =
                        gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                    gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                        &stream,
                        96,
                        96,
                        true,
                        gio::Cancellable::NONE,
                    )
                    .ok()
                });
                let targets = ICONS.with_borrow_mut(|icons| {
                    icons.active -= 1;
                    if let Some(pixbuf) = &pixbuf {
                        if icons.ready.len() >= 160 {
                            if let Some(old_key) = icons.ready.keys().next().cloned() {
                                icons.ready.remove(&old_key);
                            }
                        }
                        icons.ready.insert(key.clone(), pixbuf.clone());
                    }
                    icons.pending.remove(&key).unwrap_or_default()
                });
                if let Some(pixbuf) = pixbuf {
                    for (image, size) in targets {
                        if let Some(image) = image.upgrade() {
                            paint_icon(&image, &pixbuf, size);
                        }
                    }
                }
                load_queued_icons();
            },
        );
    }
}

fn paint_icon(image: &gtk::Image, pixbuf: &gtk::gdk_pixbuf::Pixbuf, size: i32) {
    let scale = f64::from(size) / f64::from(pixbuf.width().max(pixbuf.height()));
    if let Some(icon) = pixbuf.scale_simple(
        (f64::from(pixbuf.width()) * scale).round().max(1.0) as i32,
        (f64::from(pixbuf.height()) * scale).round().max(1.0) as i32,
        gtk::gdk_pixbuf::InterpType::Bilinear,
    ) {
        image.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&icon)));
    }
}

fn choose_install_destination(
    window: &adw::ApplicationWindow,
    container: &gtk::Box,
    open_options: &gtk::Button,
    preferred: Destination,
    (user, system): (Vec<(Remote, CatalogApp)>, Vec<(Remote, CatalogApp)>),
    name: &str,
) {
    let heading = tr("Where do you want to install {name}?").replace("{name}", name);
    let scope = gtk::DropDown::from_strings(&[tr("For my account"), tr("For the system")]);
    scope.set_selected((preferred == Destination::System) as u32);
    let repository = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let info = home_label("", "dim-label");
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 14);
    controls.add_css_class("packlift-store-install");
    controls.append(&home_label(&heading, "title-2"));
    controls.append(&labeled_control(tr("Install scope"), &scope));
    controls.append(&labeled_control(tr("Repository"), &repository));
    controls.append(&info);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = text_button(tr("Cancel"));
    let install = text_button(tr("Install"));
    install.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&install);
    controls.append(&actions);

    let choices = Rc::new([user, system]);
    let refresh = {
        let choices = choices.clone();
        let repository = repository.clone();
        move |scope: u32| {
            let options = choices[scope.min(1) as usize]
                .iter()
                .map(|(remote, _)| remote.label())
                .collect::<Vec<_>>();
            let labels = options.iter().map(String::as_str).collect::<Vec<_>>();
            repository.set_model(Some(&gtk::StringList::new(&labels)));
            repository.set_selected(0);
            repository.notify("selected");
        }
    };
    refresh(scope.selected());
    scope.connect_selected_notify(move |scope| refresh(scope.selected()));
    let weak_scope = scope.downgrade();
    let update = {
        let choices = choices.clone();
        let button = install.downgrade();
        move |repository: &gtk::DropDown| {
            let (Some(button), Some(scope)) = (button.upgrade(), weak_scope.upgrade()) else {
                return;
            };
            let selected =
                choices[scope.selected().min(1) as usize].get(repository.selected() as usize);
            button.set_sensitive(selected.is_some());
            repository.set_sensitive(selected.is_some());
            let message = match selected {
                None => tr("No repository offers this app for the selected scope."),
                Some((remote, _)) if remote.flathub && remote.name.is_empty() => {
                    tr("Packlift will add Flathub to the selected scope if needed.")
                }
                _ => "",
            };
            info.set_text(message);
            info.set_visible(!message.is_empty());
        }
    };
    update(&repository);
    repository.connect_selected_notify(update);

    let dismiss = dismiss_store_options(container, &controls, open_options);
    let close = dismiss.clone();
    cancel.connect_clicked(move |_| close());
    let target = window.downgrade();
    let weak_scope = scope.downgrade();
    let repository = repository.downgrade();
    install.connect_clicked(move |_| {
        let (Some(window), Some(repository), Some(scope)) =
            (target.upgrade(), repository.upgrade(), weak_scope.upgrade())
        else {
            return;
        };
        let Some((source, candidate)) = choices[scope.selected().min(1) as usize]
            .get(repository.selected() as usize)
            .cloned()
        else {
            return;
        };
        let Some(application) = window.application().and_downcast::<adw::Application>() else {
            return;
        };
        dismiss();
        run_job(
            &application,
            &window,
            tr("Installing program"),
            move |log| {
                let runner = Runner::new(None, log)?;
                store::install(&runner, &source, &candidate)?;
                let scope = match source.destination {
                    Destination::User => "user",
                    Destination::System => "system",
                };
                let installed = crate::installer()
                    .and_then(|manager| manager.flatpak_app_installations(&candidate.id))
                    .unwrap_or_default();
                let expected = candidate.reference.trim_start_matches("app/");
                let mut matches = installed.into_iter().filter(|record| {
                    record["flatpak_scope"].as_str() == Some(scope)
                        && (expected == candidate.id || record["target"].as_str() == Some(expected))
                });
                let record = match (matches.next(), matches.next()) {
                    (Some(record), None) => record,
                    _ => json!({
                        "name": candidate.name, "method": "flatpak", "flatpak_app_id": candidate.id,
                    }),
                };
                Ok(Some(record))
            },
        );
    });
    container.append(&controls);
    container.set_visible(true);
    open_options.set_visible(false);
    scope.grab_focus();
}

fn dismiss_store_options(
    container: &gtk::Box,
    controls: &gtk::Box,
    open_options: &gtk::Button,
) -> Rc<impl Fn()> {
    Rc::new({
        let container = container.downgrade();
        let controls = controls.downgrade();
        let open_options = open_options.downgrade();
        move || {
            if let (Some(container), Some(controls)) = (container.upgrade(), controls.upgrade()) {
                container.remove(&controls);
                container.set_visible(false);
            }
            if let Some(button) = open_options.upgrade() {
                button.set_visible(true);
                button.grab_focus();
            }
        }
    })
}

fn choose_uninstall_destination(
    window: &adw::ApplicationWindow,
    container: &gtk::Box,
    open_options: &gtk::Button,
    records: Vec<Value>,
    name: &str,
) {
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 12);
    controls.add_css_class("packlift-store-install");
    controls.append(&home_label(
        &format!("{} · {name}", tr("Uninstall")),
        "title-2",
    ));
    let labels = records
        .iter()
        .map(|record| {
            let reference = record["target"].as_str().unwrap_or_default();
            let details = reference.split_once('/').map_or("", |(_, details)| details);
            format!("{} · {details}", crate::flatpak_scope_label(record))
        })
        .collect::<Vec<_>>();
    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let scope = gtk::DropDown::from_strings(&labels);
    controls.append(&labeled_control(tr("Install scope"), &scope));

    let (delete_option, delete_data) = crate::flatpak_delete_data_option();
    controls.append(&delete_option);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = text_button(tr("Cancel"));
    let uninstall = text_button(tr("Uninstall"));
    uninstall.add_css_class("destructive-action");
    actions.append(&cancel);
    actions.append(&uninstall);
    controls.append(&actions);

    let dismiss = dismiss_store_options(container, &controls, open_options);
    let close = dismiss.clone();
    cancel.connect_clicked(move |_| close());
    let parent = window.downgrade();
    let choice = scope.downgrade();
    let delete_data = delete_data.downgrade();
    uninstall.connect_clicked(move |_| {
        let (Some(window), Some(scope)) = (parent.upgrade(), choice.upgrade()) else {
            return;
        };
        let Some(record) = records.get(scope.selected() as usize).cloned() else {
            return;
        };
        let Some(application) = window.application().and_downcast::<adw::Application>() else {
            return;
        };
        let delete_data = delete_data.upgrade().is_some_and(|check| check.is_active());
        dismiss();
        run_job(
            &application,
            &window,
            tr("Removing from your library"),
            move |log| {
                let manager = crate::installer_with_output(log)?;
                if delete_data {
                    manager.uninstall_removing_data(&record)?;
                } else {
                    manager.uninstall(&record)?;
                }
                Ok(None)
            },
        );
    });
    container.append(&controls);
    container.set_visible(true);
    open_options.set_visible(false);
    scope.grab_focus();
}

fn bind_gallery(gallery: &gtk::Box, shots: Vec<Screenshot>) {
    const PER_VIEW: usize = 2;
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let heading = home_label(tr("Screenshots"), "title-3");
    heading.set_hexpand(true);
    header.append(&heading);
    let position = home_label("", "dim-label");
    header.append(&position);
    let previous = gtk::Button::from_icon_name("go-previous-symbolic");
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    let close = gtk::Button::from_icon_name("view-restore-symbolic");
    for (button, name) in [
        (&previous, tr("Previous")),
        (&next, tr("Next")),
        (&close, tr("Close preview")),
    ] {
        button.set_tooltip_text(Some(name));
        button.update_property(&[gtk::accessible::Property::Label(name)]);
        header.append(button);
    }
    gallery.append(&header);

    let frames = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    frames.set_homogeneous(true);
    gallery.append(&frames);
    let offset = Rc::new(Cell::new(0usize));
    let expanded = Rc::new(Cell::new(false));
    let count = shots.len();
    let cache = Rc::new(RefCell::new(HashMap::new()));
    let render = Rc::new({
        let (frames, position, previous, next, close) = (
            frames.downgrade(),
            position.downgrade(),
            previous.downgrade(),
            next.downgrade(),
            close.downgrade(),
        );
        let offset = offset.clone();
        let expanded = expanded.clone();
        move |focus_preview: Option<usize>| {
            let (Some(frames), Some(position), Some(previous), Some(next), Some(close)) = (
                frames.upgrade(),
                position.upgrade(),
                previous.upgrade(),
                next.upgrade(),
                close.upgrade(),
            ) else {
                return;
            };
            let start = offset.get();
            let enlarged = expanded.get();
            let end = (start + if enlarged { 1 } else { PER_VIEW }).min(count);
            let range = if start + 1 == end {
                end.to_string()
            } else {
                format!("{}–{end}", start + 1)
            };
            position.set_text(&format!("{range} / {count}"));
            previous.set_visible(enlarged || count > PER_VIEW);
            next.set_visible(enlarged || count > PER_VIEW);
            close.set_visible(enlarged);
            previous.set_sensitive(start > 0);
            next.set_sensitive(end < count);
            while let Some(child) = frames.first_child() {
                frames.remove(&child);
            }
            for (index, shot) in shots.iter().enumerate().take(end).skip(start) {
                let (frame, button) = screenshot_frame(shot, enlarged, index, &cache);
                frames.append(&frame);
                if focus_preview == Some(index) {
                    button.grab_focus();
                }
            }
        }
    });

    let actions = gio::SimpleActionGroup::new();
    let enlarge = gio::SimpleAction::new("enlarge", Some(&u32::static_variant_type()));
    let show = render.clone();
    let start = offset.clone();
    let zoom = expanded.clone();
    let close_button = close.downgrade();
    enlarge.connect_activate(move |_, parameter| {
        let Some(index) = parameter.and_then(|value| value.get::<u32>()) else {
            return;
        };
        if index as usize >= count {
            return;
        }
        start.set(index as usize);
        zoom.set(true);
        show(None);
        if let Some(button) = close_button.upgrade() {
            button.grab_focus();
        }
    });
    actions.add_action(&enlarge);
    gallery.insert_action_group("gallery", Some(&actions));

    let collapse = Rc::new({
        let show = render.clone();
        let offset = offset.clone();
        let expanded = expanded.clone();
        move || {
            let selected = offset.get();
            expanded.set(false);
            offset.set(selected / PER_VIEW * PER_VIEW);
            show(Some(selected));
        }
    });
    let collapse_button = collapse.clone();
    close.connect_clicked(move |_| collapse_button());
    let step = Rc::new({
        let offset = offset.clone();
        let expanded = expanded.clone();
        let (previous, next, close) = (previous.downgrade(), next.downgrade(), close.downgrade());
        let show = render.clone();
        move |forward| {
            let old = offset.get();
            let increment = if expanded.get() { 1 } else { PER_VIEW };
            let new = if forward {
                old.checked_add(increment).filter(|index| *index < count)
            } else {
                old.checked_sub(increment)
            };
            let Some(new) = new else {
                return;
            };
            let focused = [previous.upgrade(), next.upgrade()]
                .into_iter()
                .flatten()
                .find(|button| button.has_focus());
            offset.set(new);
            show(None);
            if focused.is_some_and(|button| !button.is_sensitive()) {
                if let Some(button) = [next.upgrade(), previous.upgrade(), close.upgrade()]
                    .into_iter()
                    .flatten()
                    .find(|button| button.is_sensitive() && button.is_visible())
                {
                    button.grab_focus();
                }
            }
        }
    });
    let step_back = step.clone();
    previous.connect_clicked(move |_| step_back(false));
    let step_forward = step.clone();
    next.connect_clicked(move |_| step_forward(true));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        use gtk::gdk::ModifierType;
        if !expanded.get()
            || modifiers.intersects(
                ModifierType::ALT_MASK | ModifierType::CONTROL_MASK | ModifierType::SUPER_MASK,
            )
        {
            return glib::Propagation::Proceed;
        }
        match key {
            gtk::gdk::Key::Escape => collapse(),
            gtk::gdk::Key::Left => step(false),
            gtk::gdk::Key::Right => step(true),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    gallery.add_controller(keys);
    render(None);
}

type ScreenshotCache = Rc<RefCell<HashMap<String, gtk::gdk::Texture>>>;

fn screenshot_frame(
    shot: &Screenshot,
    enlarged: bool,
    index: usize,
    cache: &ScreenshotCache,
) -> (gtk::Box, gtk::Button) {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .height_request(if enlarged { 458 } else { 306 })
        .build();
    picture.add_css_class("packlift-store-shot");
    let name = if shot.caption.is_empty() {
        tr("Screenshots")
    } else {
        &shot.caption
    };
    picture.set_alternative_text(Some(name));
    let button = gtk::Button::new();
    if enlarged {
        frame.append(&picture);
    } else {
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&picture));
        let zoom = gtk::Image::from_icon_name("view-fullscreen-symbolic");
        zoom.set_halign(gtk::Align::End);
        zoom.set_valign(gtk::Align::Start);
        zoom.set_can_target(false);
        zoom.add_css_class("packlift-store-zoom");
        overlay.add_overlay(&zoom);
        button.set_child(Some(&overlay));
        button.add_css_class("flat");
        button.add_css_class("packlift-store-shot-button");
        button.set_tooltip_text(Some(tr("Enlarge screenshot")));
        button.update_property(&[
            gtk::accessible::Property::Label(name),
            gtk::accessible::Property::Description(tr("Enlarge screenshot")),
        ]);
        button.set_action_name(Some("gallery.enlarge"));
        button.set_action_target_value(Some(&(index as u32).to_variant()));
        frame.append(&button);
    }
    let caption = gtk::Label::new(Some(tr("Loading screenshots…")));
    caption.add_css_class("dim-label");
    caption.set_wrap(true);
    frame.append(&caption);
    let url = if enlarged { &shot.large_url } else { &shot.url };
    let text = shot.caption.clone();
    let finish =
        move |picture: &gtk::Picture, caption: &gtk::Label, texture: Option<&gtk::gdk::Texture>| {
            if let Some(texture) = texture {
                picture.set_paintable(Some(texture));
                caption.set_text(&text);
                caption.set_visible(!text.is_empty());
            } else {
                caption.set_text(tr("Screenshots could not be loaded."));
            }
        };
    if let Some(texture) = cache.borrow().get(url) {
        finish(&picture, &caption, Some(texture));
    } else {
        let key = url.clone();
        let url = key.clone();
        let fallback = shot.url.clone();
        let limit = if enlarged || shot.url == shot.large_url {
            1440
        } else {
            800
        };
        let (picture, caption) = (picture.downgrade(), caption.downgrade());
        let cache = cache.clone();
        background(
            move || {
                let runner = worker()?;
                store::screenshot_bytes(&runner, &url).or_else(|error| {
                    if url == fallback {
                        Err(error)
                    } else {
                        store::screenshot_bytes(&runner, &fallback)
                    }
                })
            },
            move |result| {
                let texture = result.ok().and_then(|bytes| {
                    let stream =
                        gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                    gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                        &stream,
                        limit,
                        limit,
                        true,
                        gio::Cancellable::NONE,
                    )
                    .ok()
                    .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
                });
                if let (Some(picture), Some(caption)) = (picture.upgrade(), caption.upgrade()) {
                    finish(&picture, &caption, texture.as_ref());
                }
                if let Some(texture) = texture {
                    cache.borrow_mut().insert(key, texture);
                }
            },
        );
    }
    (frame, button)
}

fn labeled_control(label: &str, control: &gtk::DropDown) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 4);
    group.set_hexpand(true);
    group.append(&home_label(label, "heading"));
    control.update_property(&[gtk::accessible::Property::Label(label)]);
    group.append(control);
    group
}

fn worker() -> packlift::core::Result<Runner> {
    Runner::new(None, Arc::new(|_| {}))
}

fn background<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = gio::spawn_blocking(job).await {
            done(result);
        }
    });
}
