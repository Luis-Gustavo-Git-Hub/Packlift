use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, VecDeque},
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
    installed: RefCell<Option<BTreeSet<String>>>,
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
        installed: RefCell::new(None),
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
    let weak = Rc::downgrade(&view);
    toolbar.connect_map(move |_| {
        if let Some(view) = weak.upgrade() {
            view.refresh_explore();
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
            move || {
                let remotes = worker().and_then(|runner| store::catalog_remotes(&runner))?;
                let installed =
                    crate::installer().and_then(|installer| installer.installed_flatpak_ids());
                Ok::<_, packlift::core::PackliftError>((remotes, installed.ok()))
            },
            move |result| match result {
                Ok((remotes, installed)) => {
                    *view.remotes.borrow_mut() = remotes;
                    *view.installed.borrow_mut() = installed;
                    view.new_search();
                }
                Err(error) => {
                    view.set_status(&error.to_string());
                    view.set_busy(false);
                }
            },
        );
    }

    fn refresh_explore(self: &Rc<Self>) {
        if !self
            .catalog_search
            .borrow()
            .as_ref()
            .is_some_and(CatalogSearch::is_explore)
        {
            return;
        }
        self.feature.set_visible(false);
        self.list.set_visible(false);
        let view = self.clone();
        background(
            move || crate::installer().and_then(|installer| installer.installed_flatpak_ids()),
            move |result| {
                if let Ok(installed) = result {
                    let changed = view.installed.borrow().as_ref() != Some(&installed);
                    *view.installed.borrow_mut() = Some(installed);
                    if changed
                        && !view.busy.get()
                        && view
                            .catalog_search
                            .borrow()
                            .as_ref()
                            .is_some_and(CatalogSearch::is_explore)
                    {
                        view.search_for(String::new());
                        return;
                    }
                }
                view.feature.set_visible(true);
                view.list.set_visible(true);
            },
        );
    }

    fn new_search(self: &Rc<Self>) {
        self.search_for(self.search.text().trim().to_owned());
    }

    fn search_for(self: &Rc<Self>, query: String) {
        self.pending_search.set(false);
        let mut search = CatalogSearch::new(self.remotes.borrow().clone(), query);
        if search.is_explore() {
            let installed = self.installed.borrow();
            let Some(installed) = installed.as_ref() else {
                self.catalog_search.borrow_mut().take();
                self.feature.set_visible(false);
                self.list.remove_all();
                self.page_label.set_text("");
                self.previous.set_sensitive(false);
                self.next.set_sensitive(false);
                self.set_status(tr("Could not check which Flatpaks are installed."));
                self.set_busy(false);
                return;
            };
            search.exclude_installed(installed);
        }
        self.feature.set_visible(true);
        self.list.set_visible(true);
        *self.catalog_search.borrow_mut() = Some(search);
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
    let install_controls = gtk::Box::new(gtk::Orientation::Vertical, 6);
    install_controls.set_valign(gtk::Align::Center);
    let repository = gtk::DropDown::new(
        Some(gtk::StringList::new(&[&remote.label()])),
        None::<gtk::Expression>,
    );
    repository.set_tooltip_text(Some(tr("Repository")));
    repository.update_property(&[gtk::accessible::Property::Label(tr("Repository"))]);
    repository.set_sensitive(false);
    install_controls.append(&repository);
    let install = gtk::MenuButton::builder()
        .label(tr("Install"))
        .direction(gtk::ArrowType::Down)
        .sensitive(false)
        .build();
    install.add_css_class("suggested-action");
    install.add_css_class("pill");
    install.set_width_request(104);
    install_controls.append(&install);
    identity.append(&install_controls);

    let uninstall = gtk::Button::with_label(tr("Uninstall"));
    uninstall.set_valign(gtk::Align::Center);
    uninstall.add_css_class("destructive-action");
    uninstall.add_css_class("pill");
    uninstall.set_visible(false);
    identity.append(&uninstall);
    hero.append(&identity);
    let install_options = gtk::Box::new(gtk::Orientation::Vertical, 0);
    install_options.set_visible(false);
    hero.append(&install_options);

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
    permissions.add_prefix(&gtk::Image::from_icon_name("security-high-symbolic"));
    permissions.set_visible(false);
    let permissions_group = gtk::ListBox::new();
    permissions_group.set_selection_mode(gtk::SelectionMode::None);
    permissions_group.add_css_class("boxed-list");
    permissions_group.append(&permissions);
    permissions_group.set_visible(false);
    hero.append(&permissions_group);
    body.append(&hero);

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
    let controls = install_controls.downgrade();
    let refresh_button = uninstall.downgrade();
    let app_id = app.id.clone();
    toolbar.connect_map(move |_| {
        let id = app_id.clone();
        let state = installed_state.clone();
        let (controls, refresh_button) = (controls.clone(), refresh_button.clone());
        background(
            move || crate::installer()?.flatpak_app_installations(&id),
            move |result| {
                let (Some(controls), Some(button)) = (controls.upgrade(), refresh_button.upgrade())
                else {
                    return;
                };
                if let Ok(records) = result {
                    let is_installed = !records.is_empty();
                    *state.borrow_mut() = records;
                    controls.set_visible(!is_installed);
                    button.set_visible(is_installed);
                    if is_installed && button.root().is_some_and(|root| root.focus().is_none()) {
                        button.grab_focus();
                    }
                }
            },
        );
    });
    let dialog = window.downgrade();
    let options = install_options.downgrade();
    uninstall.connect_clicked(move |button| {
        if let (Some(window), Some(options)) = (dialog.upgrade(), options.upgrade()) {
            choose_uninstall_destination(&window, &options, button, installed.borrow().clone());
        }
    });

    let preferred = remote.clone();
    let parent = window.downgrade();
    let (repository, install) = (repository.downgrade(), install.downgrade());
    load_detail_sources(remote, app, sources_section, sources, move |choices| {
        if let (Some(repository), Some(install)) = (repository.upgrade(), install.upgrade()) {
            bind_install_menu(&parent, &repository, &install, &preferred, choices);
        }
    });
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
                        &store_fact(
                            "folder-download-symbolic",
                            tr("Download size"),
                            &glib::format_size(size),
                            tr("Package download size reported by the repository. Runtimes may require additional downloads."),
                            None,
                        ),
                        0,
                    );
                    indicators.set_visible(true);
                }
                if let Some(monthly) = result.installs_last_month {
                    indicators.append(&store_fact(
                        "view-list-ordered-symbolic",
                        tr("Installs last month"),
                        &monthly.to_string(),
                        tr("Installations recorded by Flathub in the last month."),
                        None,
                    ));
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
                    let license = store_fact(
                        "text-x-generic-symbolic",
                        tr("License"),
                        value,
                        tr("The license defines how you may use, modify, and share this app."),
                        Some(&details.license),
                    );
                    facts.append(&license);
                    facts.set_visible(true);
                }
                if let Some(adaptive) = details.adaptive {
                    facts.append(&store_fact(
                        if adaptive {
                            "phone-symbolic"
                        } else {
                            "computer-symbolic"
                        },
                        tr("Device support"),
                        if adaptive {
                            tr("Adaptive")
                        } else {
                            tr("Desktop")
                        },
                        if adaptive {
                            tr("This app adapts to smaller screens, such as phones.")
                        } else {
                            tr("This app is designed primarily for desktop screens.")
                        },
                        None,
                    ));
                    facts.set_visible(true);
                }
                if let Some(age) = details.age {
                    facts.append(&store_fact(
                        "avatar-default-symbolic",
                        tr("Age rating"),
                        &format!("{age}+"),
                        tr("Minimum recommended age based on the app’s content rating."),
                        None,
                    ));
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

type StoreChoice = (Remote, CatalogApp);

fn load_detail_sources(
    remote: &Remote,
    app: &CatalogApp,
    section: gtk::Box,
    rows: gtk::ListBox,
    loaded: impl FnOnce(Vec<StoreChoice>) + 'static,
) {
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
            for (remote, _) in &choices {
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
            loaded(choices);
        },
    );
}

fn store_fact(
    icon: &str,
    label: &str,
    value: &str,
    description: &str,
    detail: Option<&str>,
) -> gtk::FlowBoxChild {
    let button = gtk::MenuButton::new();
    button.add_css_class("packlift-store-fact");
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{label} · {value}"
    ))]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name(icon);
    icon.add_css_class("dim-label");
    content.append(&icon);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.append(&home_label(label, "dim-label"));
    labels.append(&home_label(value, "heading"));
    content.append(&labels);
    button.set_child(Some(&content));

    let popover = gtk::Popover::new();
    let explanation = gtk::Box::new(gtk::Orientation::Vertical, 6);
    explanation.set_margin_top(4);
    explanation.set_margin_bottom(4);
    explanation.set_margin_start(4);
    explanation.set_margin_end(4);
    let title = home_label(label, "heading");
    explanation.append(&title);
    if let Some(detail) = detail {
        let detail = home_label(detail, "monospace");
        detail.set_selectable(true);
        detail.set_max_width_chars(34);
        explanation.append(&detail);
    }
    let description = home_label(description, "");
    description.set_max_width_chars(34);
    explanation.append(&description);
    popover.set_child(Some(&explanation));
    button.set_popover(Some(&popover));
    let child = gtk::FlowBoxChild::new();
    child.set_focusable(false);
    child.set_child(Some(&button));
    child
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
    background(
        move || {
            let runner = worker().ok()?;
            store::app_details(&runner, &origin, &selected)
                .ok()?
                .screenshots
                .into_iter()
                .next()
        },
        move |result| {
            let (Some(shot), Some(picture)) = (result, image.upgrade()) else {
                return;
            };
            let caption = shot.caption;
            load_screenshot(&picture, shot.url, None, move |picture, texture| {
                if let Some(texture) = texture {
                    picture.set_paintable(Some(texture));
                    if !caption.is_empty() {
                        picture.set_alternative_text(Some(&caption));
                    }
                    picture.set_visible(true);
                }
            });
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
    if let Some(source) = app.icon.clone() {
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
                    .any(|image| media_is_visible(&image))
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
                        256,
                        256,
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
    image.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(pixbuf)));
    image.set_pixel_size(size);
}

fn media_is_visible(widget: &impl IsA<gtk::Widget>) -> bool {
    if !widget.is_mapped() {
        return false;
    }
    let mut ancestor = widget.parent();
    while let Some(parent) = ancestor {
        if parent.is::<gtk::ScrolledWindow>()
            && !widget.compute_bounds(&parent).is_some_and(|bounds| {
                bounds.x() + bounds.width() > 0.0
                    && bounds.x() < parent.width() as f32
                    && bounds.y() + bounds.height() > 0.0
                    && bounds.y() < parent.height() as f32
            })
        {
            return false;
        }
        ancestor = parent.parent();
    }
    true
}

struct InstallSource {
    remote: Remote,
    scopes: [Option<StoreChoice>; 2],
}

fn same_repository(left: &Remote, right: &Remote) -> bool {
    (left.flathub && right.flathub)
        || (left.url.trim_end_matches('/') == right.url.trim_end_matches('/')
            && (!left.url.is_empty() || left.name == right.name))
}

fn bind_install_menu(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    repository: &gtk::DropDown,
    install: &gtk::MenuButton,
    preferred: &Remote,
    choices: Vec<StoreChoice>,
) {
    let mut sources: Vec<InstallSource> = Vec::new();
    for choice in choices {
        let remote = &choice.0;
        let index = sources
            .iter()
            .position(|source| same_repository(&source.remote, remote))
            .unwrap_or_else(|| {
                sources.push(InstallSource {
                    remote: remote.clone(),
                    scopes: [None, None],
                });
                sources.len() - 1
            });
        let scope = usize::from(remote.destination == Destination::System);
        if sources[index].scopes[scope].is_none() {
            sources[index].scopes[scope] = Some(choice);
        }
    }
    if sources.is_empty() {
        repository.set_model(None::<&gtk::StringList>);
        install.set_tooltip_text(Some(tr("No repositories offer this app.")));
        return;
    }
    let labels = sources
        .iter()
        .map(|source| source.remote.label())
        .collect::<Vec<_>>();
    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let selected = sources
        .iter()
        .position(|source| same_repository(&source.remote, preferred))
        .unwrap_or(0);
    repository.set_model(Some(&gtk::StringList::new(&labels)));
    repository.set_selected(selected as u32);
    repository.set_sensitive(true);
    let parent = window.clone();
    let button = install.downgrade();
    let update = move |repository: &gtk::DropDown| {
        if let (Some(button), Some(source)) = (
            button.upgrade(),
            sources.get(repository.selected() as usize),
        ) {
            button.set_popover(Some(&install_scope_popover(&parent, source)));
            button.set_sensitive(true);
            if button.is_mapped() && button.root().is_some_and(|root| root.focus().is_none()) {
                button.grab_focus();
            }
        }
    };
    update(repository);
    repository.connect_selected_notify(update);
}

fn install_scope_popover(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    source: &InstallSource,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_offset(-56, 0);
    let options = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let heading = home_label(tr("Who should this app be installed for?"), "heading");
    heading.set_max_width_chars(28);
    options.append(&heading);
    for (label, choice) in [tr("For my account"), tr("For the system")]
        .into_iter()
        .zip(&source.scopes)
    {
        let button = gtk::Button::with_label(label);
        button.add_css_class("flat");
        button.set_sensitive(choice.is_some());
        if let Some((remote, candidate)) = choice.clone() {
            let window = window.clone();
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                if let (Some(window), Some(popover)) = (window.upgrade(), popover.upgrade()) {
                    popover.popdown();
                    install_store_app(&window, remote.clone(), candidate.clone());
                }
            });
        } else {
            button.set_tooltip_text(Some(tr(
                "No repository offers this app for the selected scope.",
            )));
        }
        options.append(&button);
    }
    if source
        .scopes
        .iter()
        .flatten()
        .any(|(remote, _)| remote.flathub && remote.name.is_empty())
    {
        let note = home_label(
            tr("Packlift will add Flathub to the selected scope if needed."),
            "dim-label",
        );
        note.set_max_width_chars(34);
        options.append(&note);
    }
    popover.set_child(Some(&options));
    popover
}

fn install_store_app(window: &adw::ApplicationWindow, source: Remote, candidate: CatalogApp) {
    let Some(application) = window.application().and_downcast::<adw::Application>() else {
        return;
    };
    run_job(&application, window, tr("Installing program"), move |log| {
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
    });
}

fn inline_store_options() -> gtk::Box {
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 10);
    controls.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    controls
}

fn store_options_actions(
    controls: &gtk::Box,
    dismiss: Rc<impl Fn() + 'static>,
    action: &str,
    style: &str,
) -> gtk::Button {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = text_button(tr("Cancel"));
    cancel.add_css_class("flat");
    let close = dismiss.clone();
    cancel.connect_clicked(move |_| close());
    actions.append(&cancel);
    let confirm = text_button(action);
    confirm.add_css_class(style);
    confirm.add_css_class("pill");
    actions.append(&confirm);
    controls.append(&actions);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            dismiss();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    controls.add_controller(keys);
    confirm
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
) {
    let controls = inline_store_options();
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

    let dismiss = dismiss_store_options(container, &controls, open_options);
    let uninstall = store_options_actions(
        &controls,
        dismiss.clone(),
        tr("Uninstall"),
        "destructive-action",
    );
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

struct ScreenshotGallery {
    shots: Vec<Screenshot>,
    selected: Cell<Option<usize>>,
    strip: gtk::Adjustment,
    stack: glib::WeakRef<gtk::Stack>,
    preview: glib::WeakRef<gtk::Box>,
    position: glib::WeakRef<gtk::Label>,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    close: glib::WeakRef<gtk::Button>,
    thumbnails: Vec<glib::WeakRef<gtk::Button>>,
}

impl ScreenshotGallery {
    fn update_controls(&self) {
        let (Some(position), Some(previous), Some(next), Some(close)) = (
            self.position.upgrade(),
            self.previous.upgrade(),
            self.next.upgrade(),
            self.close.upgrade(),
        ) else {
            return;
        };
        let lower = self.strip.lower();
        let upper = self.strip.upper() - self.strip.page_size();
        let scrollable = upper - lower > 1.0;
        let zoomed = self.selected.get();
        let had_previous = previous.has_focus();
        let had_next = next.has_focus();
        let (can_go_back, can_go_forward) = match zoomed {
            Some(index) => (index > 0, index + 1 < self.shots.len()),
            None => (
                self.strip.value() - lower > 1.0,
                upper - self.strip.value() > 1.0,
            ),
        };
        position.set_visible(zoomed.is_some());
        if let Some(index) = zoomed {
            position.set_text(&format!("{} / {}", index + 1, self.shots.len()));
        }
        previous.set_visible(if zoomed.is_some() {
            self.shots.len() > 1
        } else {
            scrollable
        });
        next.set_visible(previous.is_visible());
        previous.set_sensitive(can_go_back);
        next.set_sensitive(can_go_forward);
        close.set_visible(zoomed.is_some());
        if had_previous && !can_go_back && can_go_forward {
            next.grab_focus();
        } else if had_next && !can_go_forward && can_go_back {
            previous.grab_focus();
        }
    }

    fn enlarge(&self, index: usize) {
        let (Some(stack), Some(preview), Some(shot)) = (
            self.stack.upgrade(),
            self.preview.upgrade(),
            self.shots.get(index),
        ) else {
            return;
        };
        if let Some(old) = preview.first_child() {
            preview.remove(&old);
        }
        preview.append(&screenshot_frame(shot, None));
        self.selected.set(Some(index));
        stack.set_visible_child_name("preview");
        self.update_controls();
    }

    fn collapse(&self) {
        let (Some(stack), Some(index)) = (self.stack.upgrade(), self.selected.take()) else {
            return;
        };
        stack.set_visible_child_name("strip");
        self.update_controls();
        if let Some(button) = self.thumbnails.get(index).and_then(glib::WeakRef::upgrade) {
            button.grab_focus();
        }
    }

    fn step(&self, forward: bool) {
        if let Some(index) = self.selected.get() {
            let next = if forward {
                index.checked_add(1)
            } else {
                index.checked_sub(1)
            };
            if let Some(next) = next.filter(|index| *index < self.shots.len()) {
                self.enlarge(next);
            }
        } else {
            let distance = self.strip.page_size() * 0.8;
            let value = self.strip.value() + if forward { distance } else { -distance };
            self.strip.set_value(value);
        }
    }
}

fn bind_gallery(gallery: &gtk::Box, shots: Vec<Screenshot>) {
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

    let previews = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let viewport = gtk::Viewport::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    viewport.set_scroll_to_focus(true);
    viewport.set_child(Some(&previews));
    let strip = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&viewport)
        .build();
    let enlarged = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let stack = gtk::Stack::new();
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.add_named(&strip, Some("strip"));
    stack.add_named(&enlarged, Some("preview"));
    gallery.append(&stack);

    let thumbnails = shots
        .iter()
        .map(|shot| {
            let button = gtk::Button::new();
            previews.append(&screenshot_frame(shot, Some(&button)));
            button
        })
        .collect::<Vec<_>>();
    let view = Rc::new(ScreenshotGallery {
        shots,
        selected: Cell::new(None),
        strip: strip.hadjustment(),
        stack: stack.downgrade(),
        preview: enlarged.downgrade(),
        position: position.downgrade(),
        previous: previous.downgrade(),
        next: next.downgrade(),
        close: close.downgrade(),
        thumbnails: thumbnails.iter().map(ObjectExt::downgrade).collect(),
    });
    for (index, button) in thumbnails.iter().enumerate() {
        let view = view.clone();
        button.connect_clicked(move |_| {
            view.enlarge(index);
            if let Some(close) = view.close.upgrade() {
                close.grab_focus();
            }
        });
    }
    {
        let view = view.clone();
        close.connect_clicked(move |_| view.collapse());
    }
    for (button, forward) in [(&previous, false), (&next, true)] {
        let view = view.clone();
        button.connect_clicked(move |_| view.step(forward));
    }
    let state = Rc::downgrade(&view);
    view.strip.connect_changed(move |_| {
        if let Some(view) = state.upgrade() {
            view.update_controls();
        }
    });
    let state = Rc::downgrade(&view);
    view.strip.connect_value_changed(move |_| {
        if let Some(view) = state.upgrade() {
            view.update_controls();
        }
    });
    view.update_controls();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        use gtk::gdk::ModifierType;
        if view.selected.get().is_none()
            || modifiers.intersects(
                ModifierType::ALT_MASK | ModifierType::CONTROL_MASK | ModifierType::SUPER_MASK,
            )
        {
            return glib::Propagation::Proceed;
        }
        match key {
            gtk::gdk::Key::Escape => view.collapse(),
            gtk::gdk::Key::Left => view.step(false),
            gtk::gdk::Key::Right => view.step(true),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    gallery.add_controller(keys);
}

fn screenshot_frame(shot: &Screenshot, thumbnail: Option<&gtk::Button>) -> gtk::Box {
    const THUMBNAIL_HEIGHT: i32 = 260;
    let enlarged = thumbnail.is_none();
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .height_request(if enlarged { 458 } else { 0 })
        .build();
    picture.add_css_class("packlift-store-shot");
    let name = if shot.caption.is_empty() {
        tr("Screenshots")
    } else {
        &shot.caption
    };
    picture.set_alternative_text(Some(name));
    if let Some(button) = thumbnail {
        let overlay = gtk::Overlay::new();
        let canvas = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let width = (shot.aspect_ratio.unwrap_or(1.6) * f64::from(THUMBNAIL_HEIGHT))
            .round()
            .clamp(118.0, 540.0) as i32;
        canvas.set_size_request(width, THUMBNAIL_HEIGHT);
        overlay.set_child(Some(&canvas));
        overlay.add_overlay(&picture);
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
        frame.append(button);
    } else {
        frame.append(&picture);
    }
    let caption = gtk::Label::new(Some(tr("Loading screenshots…")));
    caption.add_css_class("dim-label");
    if enlarged {
        caption.set_wrap(true);
    } else {
        caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
        caption.set_max_width_chars(22);
        caption.set_tooltip_text((!shot.caption.is_empty()).then_some(shot.caption.as_str()));
    }
    frame.append(&caption);
    let (url, fallback) = if enlarged && shot.large_url != shot.url {
        (shot.large_url.clone(), Some(shot.url.clone()))
    } else {
        (shot.url.clone(), None)
    };
    let text = shot.caption.clone();
    let status = caption.downgrade();
    load_screenshot(&picture, url, fallback, move |picture, texture| {
        let Some(caption) = status.upgrade() else {
            return;
        };
        if let Some(texture) = texture {
            picture.set_paintable(Some(texture));
            caption.set_text(&text);
            caption.set_visible(!text.is_empty());
        } else {
            caption.set_text(tr("Screenshots could not be loaded."));
        }
    });
    frame
}

type ScreenshotLoaded = Box<dyn FnOnce(&gtk::Picture, Option<&gtk::gdk::Texture>)>;

struct ScreenshotRequest {
    picture: glib::WeakRef<gtk::Picture>,
    loaded: ScreenshotLoaded,
}

#[derive(Default)]
struct ScreenshotQueue {
    ready: VecDeque<(String, gtk::gdk::Texture)>,
    pending: HashMap<String, Vec<ScreenshotRequest>>,
    queue: VecDeque<(String, Option<String>)>,
    active: usize,
    scheduled: bool,
}

thread_local! {
    static SCREENSHOTS: RefCell<ScreenshotQueue> = RefCell::new(ScreenshotQueue::default());
}

fn load_screenshot(
    picture: &gtk::Picture,
    url: String,
    fallback: Option<String>,
    loaded: impl FnOnce(&gtk::Picture, Option<&gtk::gdk::Texture>) + 'static,
) {
    let cached = SCREENSHOTS.with_borrow_mut(|queue| {
        let index = queue.ready.iter().position(|(key, _)| *key == url)?;
        let entry = queue.ready.remove(index)?;
        let texture = entry.1.clone();
        queue.ready.push_back(entry);
        Some(texture)
    });
    if let Some(texture) = cached {
        loaded(picture, Some(&texture));
        return;
    }
    SCREENSHOTS.with_borrow_mut(|queue| {
        let pending = queue.pending.entry(url.clone()).or_insert_with(|| {
            queue.queue.push_back((url, fallback));
            Vec::new()
        });
        pending.push(ScreenshotRequest {
            picture: picture.downgrade(),
            loaded: Box::new(loaded),
        });
        if !queue.scheduled {
            queue.scheduled = true;
            glib::idle_add_local_once(pump_screenshots);
        }
    });
}

fn pump_screenshots() {
    const MAX_ACTIVE: usize = 4;
    let jobs = SCREENSHOTS.with_borrow_mut(|queue| {
        queue.scheduled = false;
        let mut jobs = Vec::new();
        while queue.active < MAX_ACTIVE && !queue.queue.is_empty() {
            let first = queue.queue.iter().position(|(url, _)| {
                queue.pending[url]
                    .iter()
                    .filter_map(|request| request.picture.upgrade())
                    .any(|picture| media_is_visible(&picture))
            });
            let (url, fallback) = queue.queue.remove(first.unwrap_or(0)).unwrap();
            if queue.pending[&url]
                .iter()
                .all(|request| request.picture.upgrade().is_none())
            {
                queue.pending.remove(&url);
                continue;
            }
            queue.active += 1;
            jobs.push((url, fallback));
        }
        jobs
    });
    for (url, fallback) in jobs {
        let download = url.clone();
        background(
            move || {
                let runner = worker()?;
                match store::screenshot_bytes(&runner, &download) {
                    Ok(bytes) => Ok((bytes, true)),
                    Err(error) => fallback.map_or(Err(error), |fallback| {
                        store::screenshot_bytes(&runner, &fallback).map(|bytes| (bytes, false))
                    }),
                }
            },
            move |result: packlift::core::Result<(Vec<u8>, bool)>| {
                let texture = result.ok().and_then(|(bytes, full_quality)| {
                    let stream =
                        gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                    gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                        &stream,
                        2048,
                        2048,
                        true,
                        gio::Cancellable::NONE,
                    )
                    .ok()
                    .map(|pixbuf| (gtk::gdk::Texture::for_pixbuf(&pixbuf), full_quality))
                });
                let pending = SCREENSHOTS.with_borrow_mut(|queue| {
                    queue.active -= 1;
                    if let Some((texture, true)) = &texture {
                        queue.ready.push_back((url.clone(), texture.clone()));
                        let bytes = |image: &gtk::gdk::Texture| {
                            i64::from(image.width()) * i64::from(image.height()) * 4
                        };
                        let mut usage = queue
                            .ready
                            .iter()
                            .map(|(_, texture)| bytes(texture))
                            .sum::<i64>();
                        while usage > 48 * 1024 * 1024 {
                            let Some((_, old)) = queue.ready.pop_front() else {
                                break;
                            };
                            usage -= bytes(&old);
                        }
                    }
                    queue.pending.remove(&url).unwrap_or_default()
                });
                for request in pending {
                    if let Some(picture) = request.picture.upgrade() {
                        (request.loaded)(&picture, texture.as_ref().map(|(image, _)| image));
                    }
                }
                pump_screenshots();
            },
        );
    }
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
