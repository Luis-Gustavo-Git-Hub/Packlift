use std::{
    cell::{Cell, RefCell},
    collections::{hash_map::RandomState, BTreeSet, HashMap, VecDeque},
    hash::BuildHasher,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use packlift::{
    store::{self, CatalogApp, CatalogIcon, CatalogSearch, Destination, Remote, Screenshot},
    system::Runner,
};
use serde_json::{json, Value};

use crate::{
    back_to_previous, home_label, i18n::tr, page_navigation, page_shortcut, render_home,
    run_job_with_progress, show_host_access_dialog, text_button, PageReturn,
};

type StoreChoice = (Remote, CatalogApp);

struct StoreView {
    window: glib::WeakRef<adw::ApplicationWindow>,
    search: gtk::SearchEntry,
    category_tiles: gtk::FlowBox,
    categories: gtk::Box,
    discover_heading: gtk::Label,
    feature: gtk::Box,
    carousel: adw::Carousel,
    list: gtk::FlowBox,
    status: gtk::Label,
    loading_screen: gtk::Box,
    loading_status: gtk::Label,
    scroll: gtk::ScrolledWindow,
    previous: gtk::Button,
    next: gtk::Button,
    page_label: gtk::Label,
    remotes: RefCell<Vec<Remote>>,
    installed: Rc<RefCell<Option<BTreeSet<String>>>>,
    catalog_search: RefCell<Option<CatalogSearch>>,
    explore_search: RefCell<Option<CatalogSearch>>,
    banners: RefCell<Vec<FeaturedApp>>,
    page_colors: RefCell<HashMap<String, usize>>,
    banner_requested: RefCell<BTreeSet<String>>,
    page: Cell<usize>,
    busy: Cell<bool>,
    pending_search: Cell<bool>,
}

#[derive(Clone)]
struct StoreSession {
    window: glib::WeakRef<adw::ApplicationWindow>,
    toolbar: adw::ToolbarView,
    search: gtk::SearchEntry,
}

thread_local! {
    static STORE_SESSIONS: RefCell<Vec<StoreSession>> = const { RefCell::new(Vec::new()) };
    static DETAIL_CACHE: RefCell<VecDeque<(String, Instant, store::AppDetails, store::FlathubFacts)>> = const { RefCell::new(VecDeque::new()) };
    static CARD_COLORS: RandomState = RandomState::new();
}

fn store_session(app: &adw::Application, window: &adw::ApplicationWindow) -> StoreSession {
    if let Some(session) = STORE_SESSIONS.with_borrow(|sessions| {
        sessions
            .iter()
            .find(|session| session.window.upgrade().as_ref() == Some(window))
            .cloned()
    }) {
        return session;
    }
    let session = create_store(app, window);
    STORE_SESSIONS.with_borrow_mut(|sessions| sessions.push(session.clone()));
    window.connect_destroy(|window| {
        STORE_SESSIONS.with_borrow_mut(|sessions| {
            sessions.retain(|session| {
                session
                    .window
                    .upgrade()
                    .is_some_and(|owner| owner != *window)
            });
        });
    });
    session
}

pub(super) fn preload(app: &adw::Application, window: &adw::ApplicationWindow) {
    store_session(app, window);
}

pub(super) fn show(app: &adw::Application, window: &adw::ApplicationWindow) {
    if !packlift::system::host_access_available() {
        show_host_access_dialog(window);
        return;
    }
    let session = store_session(app, window);
    window.set_widget_name("packlift-store");
    window.set_title(Some(tr("Store")));
    window.set_content(Some(&session.toolbar));
    window.present();
    session.search.grab_focus();
}

pub(super) fn show_search(app: &adw::Application, window: &adw::ApplicationWindow, query: &str) {
    show(app, window);
    if window.widget_name() == "packlift-store" {
        store_session(app, window).search.set_text(query);
    }
}

pub(super) fn show_search_result(
    application: &adw::Application,
    window: &adw::ApplicationWindow,
    candidate: &CatalogApp,
) {
    show(application, window);
    if window.widget_name() == "packlift-store" {
        show_app(
            window,
            &store::flathub_remote(Destination::User),
            candidate,
            app_color(&candidate.id),
        );
    }
}

fn favorites_path() -> std::path::PathBuf {
    packlift::system::packlift_data_dir().join("store-favorites.json")
}

fn read_favorites() -> packlift::core::Result<Vec<CatalogApp>> {
    match std::fs::read(favorites_path()) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn set_favorite_icon(button: &gtk::Button, saved: bool) {
    let (icon, label) = if saved {
        ("starred-symbolic", tr("Remove from favorites"))
    } else {
        ("non-starred-symbolic", tr("Add to favorites"))
    };
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn favorite_button(app: &CatalogApp, window: &adw::ApplicationWindow) -> gtk::Button {
    let button = gtk::Button::new();
    let candidate = app.id.clone();
    button.connect_map(move |button| {
        let saved = read_favorites().is_ok_and(|apps| apps.iter().any(|app| app.id == candidate));
        set_favorite_icon(button, saved);
    });

    let app = app.clone();
    let owner = window.downgrade();
    button.connect_clicked(move |button| {
        let result = (|| -> packlift::core::Result<bool> {
            let mut apps = read_favorites()?;
            let local = if let Some(index) = apps.iter().position(|item| item.id == app.id) {
                apps.remove(index);
                false
            } else {
                apps.push(app.clone());
                true
            };
            std::fs::create_dir_all(packlift::system::packlift_data_dir())?;
            glib::file_set_contents(favorites_path(), &serde_json::to_vec(&apps)?)
                .map_err(|error| packlift::core::PackliftError::Message(error.to_string()))?;
            Ok(local)
        })();
        match result {
            Ok(local) => set_favorite_icon(button, local),
            Err(error) => {
                if let Some(window) = owner.upgrade() {
                    crate::show_error(&window, &error.to_string());
                }
            }
        }
    });
    button
}

fn show_favorites(window: &adw::ApplicationWindow) {
    let (toolbar, header) =
        detail_page_toolbar(window, tr("Favorites"), PageReturn::capture(window));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let account_title = home_label(tr("Flathub favorites"), "title-3");
    account_title.set_hexpand(true);
    controls.append(&account_title);
    let account = gtk::LinkButton::with_label(
        "https://flathub.org/my-flathub",
        tr("Open Flathub in Browser"),
    );
    controls.append(&account);
    body.append(&controls);
    body.append(&home_label(tr("Saved on this device"), "title-3"));
    let status = home_label("", "dim-label");
    body.append(&status);
    let tiles = discovery_grid();
    body.append(&tiles);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(
            &adw::Clamp::builder()
                .maximum_size(1180)
                .child(&body)
                .build(),
        )
        .build();
    toolbar.set_content(Some(&scroll));
    crate::updates_ui::attach(&toolbar, &header, window);

    let owner = window.downgrade();
    toolbar.connect_map(move |_| {
        tiles.remove_all();
        match read_favorites() {
            Ok(mut apps) => {
                apps.sort_by_cached_key(|app| app.name.to_lowercase());
                status.set_visible(apps.is_empty());
                status.set_text(tr("No favorites yet"));
                for app in apps {
                    let color = app_color(&app.id);
                    tiles.append(&app_card(
                        &owner,
                        store::flathub_remote(Destination::User),
                        app,
                        color,
                    ));
                }
            }
            Err(error) => {
                status.set_visible(true);
                status.set_text(&error.to_string());
            }
        }
    });
    window.set_widget_name("packlift-store-favorites");
    window.set_title(Some(tr("Favorites")));
    window.set_content(Some(&toolbar));
}

fn create_store(app: &adw::Application, window: &adw::ApplicationWindow) -> StoreSession {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Store"), "")));
    let home = window.downgrade();
    let application = app.downgrade();
    page_navigation(
        &toolbar,
        &header,
        tr("Library"),
        "view-grid-symbolic",
        move || {
            if let (Some(app), Some(window)) = (application.upgrade(), home.upgrade()) {
                render_home(&app, &window);
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
    search.set_search_delay(350);
    search_row.append(&search);
    let favorites = gtk::Button::from_icon_name("starred-symbolic");
    favorites.set_tooltip_text(Some(tr("Favorites")));
    favorites.update_property(&[gtk::accessible::Property::Label(tr("Favorites"))]);
    let owner = window.downgrade();
    favorites.connect_clicked(move |_| {
        if let Some(window) = owner.upgrade() {
            show_favorites(&window);
        }
    });
    search_row.append(&favorites);
    body.append(&search_row);

    let status = home_label("", "dim-label");
    status.set_selectable(true);
    body.append(&status);
    let (loading_screen, loading_status) = store_loading_screen();
    loading_screen.set_visible(false);
    let (feature, carousel) = store_carousel();
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
    let category_tiles = discovery_grid();
    catalog.append(&category_tiles);
    catalog.append(&loading_screen);
    let discover_heading = home_label(tr("Trending"), "title-2");
    discover_heading.set_visible(false);
    catalog.append(&discover_heading);
    catalog.append(&list);
    let categories = gtk::Box::new(gtk::Orientation::Vertical, 22);
    catalog.append(&categories);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&catalog)
        .build();
    body.append(&scroll);
    let (_nav, previous, page_label, next) = store_pagination();
    let layout = adw::Clamp::builder()
        .maximum_size(1180)
        .tightening_threshold(1000)
        .child(&body)
        .build();
    toolbar.set_content(Some(&layout));
    crate::updates_ui::attach(&toolbar, &header, window);
    let search_target = search.downgrade();
    page_shortcut(&toolbar, "<Control>f", move || {
        if let Some(search) = search_target.upgrade() {
            search.grab_focus();
        }
    });

    let view = Rc::new(StoreView {
        window: window.downgrade(),
        search,
        category_tiles,
        categories,
        discover_heading,
        feature,
        carousel,
        list,
        status,
        loading_screen,
        loading_status,
        scroll,
        previous,
        next,
        page_label,
        remotes: RefCell::new(Vec::new()),
        installed: Rc::new(RefCell::new(None)),
        catalog_search: RefCell::new(None),
        explore_search: RefCell::new(None),
        banners: RefCell::new(Vec::new()),
        page_colors: RefCell::new(HashMap::new()),
        banner_requested: RefCell::new(BTreeSet::new()),
        page: Cell::new(1),
        busy: Cell::new(false),
        pending_search: Cell::new(false),
    });
    let weak = Rc::downgrade(&view);
    view.search.connect_search_changed(move |_| {
        if let Some(view) = weak.upgrade() {
            if view.busy.get() {
                view.pending_search.set(true);
            } else {
                view.new_search();
            }
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
    view.build_categories();
    view.load_remotes();
    StoreSession {
        window: window.downgrade(),
        toolbar,
        search: view.search.clone(),
    }
}

fn store_loading_screen() -> (gtk::Box, gtk::Label) {
    let screen = gtk::Box::new(gtk::Orientation::Vertical, 22);
    screen.add_css_class("packlift-store-loading");
    screen.set_hexpand(true);
    screen.set_vexpand(true);
    screen.set_valign(gtk::Align::Center);

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    heading.set_halign(gtk::Align::Center);
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(24, 24);
    spinner.start();
    heading.append(&spinner);
    let status = home_label(tr("Searching the store…"), "title-3");
    heading.append(&status);
    screen.append(&heading);

    let cards = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .homogeneous(true)
        .row_spacing(10)
        .column_spacing(10)
        .build();
    for _ in 0..6 {
        cards.append(&store_loading_card());
    }
    screen.append(&cards);
    (screen, status)
}

fn store_loading_card() -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("packlift-store-skeleton-card");
    card.set_size_request(190, 82);

    let icon = gtk::Box::new(gtk::Orientation::Vertical, 0);
    icon.add_css_class("packlift-store-skeleton-icon");
    icon.set_size_request(48, 48);
    card.append(&icon);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 10);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let name = gtk::Box::new(gtk::Orientation::Vertical, 0);
    name.add_css_class("packlift-store-skeleton-line");
    name.set_size_request(100, 11);
    let description = gtk::Box::new(gtk::Orientation::Vertical, 0);
    description.add_css_class("packlift-store-skeleton-line");
    description.set_size_request(72, 9);
    text.append(&name);
    text.append(&description);
    card.append(&text);
    card
}

impl StoreView {
    fn set_status(&self, message: &str) {
        self.status.set_text(message);
        if self.busy.get() && self.loading_screen.is_visible() {
            self.loading_status.set_text(message);
            self.status.set_visible(false);
        } else {
            self.status.set_visible(!message.is_empty());
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        if busy {
            self.previous.set_sensitive(false);
            self.next.set_sensitive(false);
        } else {
            self.loading_screen.set_visible(false);
            self.scroll.set_visible(true);
            self.status.set_visible(!self.status.text().is_empty());
        }
    }

    fn load_remotes(self: &Rc<Self>) {
        self.set_busy(true);
        self.set_status(tr("Loading repositories…"));
        self.loading_screen.set_visible(true);
        self.loading_status.set_text(tr("Loading repositories…"));
        self.status.set_visible(false);
        let view = self.clone();
        background(
            move || worker().and_then(|runner| store::catalog_remotes(&runner, Destination::User)),
            move |result| {
                *view.remotes.borrow_mut() =
                    result.unwrap_or_else(|_| vec![store::flathub_remote(Destination::User)]);
                view.new_search();
                view.load_installed();
                view.load_system_remotes();
            },
        );
    }

    fn load_installed(self: &Rc<Self>) {
        let view = self.clone();
        background(
            move || crate::installer().and_then(|installer| installer.installed_flatpak_ids()),
            move |result| {
                let Ok(installed) = result else {
                    return;
                };
                if view.installed.borrow().as_ref() == Some(&installed) {
                    return;
                }
                *view.installed.borrow_mut() = Some(installed.clone());
                let explore = {
                    let mut current = view.catalog_search.borrow_mut();
                    let search = current.as_mut().filter(|search| search.is_explore());
                    search
                        .map(|search| search.exclude_installed(&installed))
                        .is_some()
                };
                if explore && !view.busy.get() {
                    view.load_catalog_page(view.page.get(), true);
                }
            },
        );
    }

    fn load_system_remotes(self: &Rc<Self>) {
        let view = self.clone();
        background(
            move || {
                worker().and_then(|runner| store::catalog_remotes(&runner, Destination::System))
            },
            move |result| {
                let Ok(system_remotes) = result else {
                    return;
                };
                let remotes = merge_catalog_remotes(view.remotes.borrow().clone(), system_remotes);
                let initial_len = view.remotes.borrow().len();
                if remotes.len() != initial_len {
                    *view.remotes.borrow_mut() = remotes;
                    let remotes = view.remotes.borrow().clone();
                    if let Some(search) = view.catalog_search.borrow_mut().as_mut() {
                        search.add_remotes(&remotes);
                    }
                    if let Some(search) = view.explore_search.borrow_mut().as_mut() {
                        search.add_remotes(&remotes);
                    }
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
        self.load_installed();
    }

    fn new_search(self: &Rc<Self>) {
        self.search_for(self.search.text().trim().to_owned());
    }

    fn search_for(self: &Rc<Self>, query: String) {
        self.pending_search.set(false);
        if let Some(active) = self
            .catalog_search
            .borrow_mut()
            .take()
            .filter(CatalogSearch::is_explore)
        {
            *self.explore_search.borrow_mut() = Some(active);
        }
        let explore = query.is_empty();
        let mut search = if explore {
            self.explore_search.borrow_mut().take()
        } else {
            None
        }
        .unwrap_or_else(|| CatalogSearch::new(self.remotes.borrow().clone(), query));
        if search.is_explore() {
            let installed = self.installed.borrow();
            if let Some(installed) = installed.as_ref() {
                search.exclude_installed(installed);
            }
        }
        self.list.set_visible(true);
        *self.catalog_search.borrow_mut() = Some(search);
        self.load_page(1);
    }

    fn load_page(self: &Rc<Self>, page: usize) {
        self.load_catalog_page(page, false);
    }

    fn load_catalog_page(self: &Rc<Self>, page: usize, keep_position: bool) {
        let Some(mut catalog_search) = self.catalog_search.borrow_mut().take() else {
            return;
        };
        self.set_busy(true);
        let explore = catalog_search.is_explore() && page == 1;
        self.category_tiles.set_visible(explore);
        self.categories.set_visible(explore);
        if !keep_position {
            self.clear_results();
            let adjustment = self.scroll.vadjustment();
            adjustment.set_value(adjustment.lower());
            self.page_label.set_text("");
            self.set_status(tr("Searching the store…"));
            self.loading_status.set_text(tr("Searching the store…"));
            self.loading_screen.set_visible(true);
            self.status.set_visible(false);
        }
        let view = self.clone();
        background(
            move || {
                let result = worker().and_then(|runner| catalog_search.page(&runner, page));
                (catalog_search, result)
            },
            move |(catalog_search, result)| {
                let mut catalog_search = catalog_search;
                catalog_search.add_remotes(&view.remotes.borrow());
                if let Some(installed) = view.installed.borrow().as_ref() {
                    catalog_search.exclude_installed(installed);
                }
                *view.catalog_search.borrow_mut() = Some(catalog_search);
                if view.pending_search.get() {
                    view.new_search();
                    return;
                }
                view.set_busy(false);
                match result {
                    Ok(mut results) => {
                        if explore {
                            if let Some(installed) = view.installed.borrow().as_ref() {
                                results.apps.retain(|(_, app)| !installed.contains(&app.id));
                            }
                        }
                        if keep_position {
                            view.clear_results();
                        }
                        view.page.set(page);
                        if !keep_position {
                            let adjustment = view.scroll.vadjustment();
                            adjustment.set_value(adjustment.lower());
                        }
                        view.previous.set_sensitive(page > 1);
                        view.next.set_sensitive(results.has_next);
                        view.page_label
                            .set_text(&tr("Page {number}").replace("{number}", &page.to_string()));
                        view.set_status(if results.apps.is_empty() {
                            tr("No apps match this search.")
                        } else {
                            ""
                        });
                        let first_color = results
                            .apps
                            .first()
                            .map_or(0, |(_, app)| app_color(&app.id));
                        *view.page_colors.borrow_mut() = results
                            .apps
                            .iter()
                            .enumerate()
                            .map(|(index, (_, app))| {
                                (app.id.clone(), (first_color + 5 * index) % APP_COLORS.len())
                            })
                            .collect();
                        view.discover_heading
                            .set_visible(explore && !results.apps.is_empty());
                        let featured_id = if explore {
                            let featured = view.render_banner(&results.apps);
                            view.prepare_banner(&results.apps);
                            featured
                        } else {
                            None
                        };
                        for (remote, app) in results.apps {
                            if featured_id.as_deref() != Some(app.id.as_str()) {
                                let color = view.page_colors.borrow()[&app.id];
                                view.list
                                    .append(&app_card(&view.window, remote, app, color));
                            }
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

    fn clear_results(&self) {
        self.list.remove_all();
        let slides: Vec<_> = (0..self.carousel.n_pages())
            .map(|index| self.carousel.nth_page(index))
            .collect();
        for slide in slides {
            self.carousel.remove(&slide);
        }
        self.feature.set_visible(false);
        self.discover_heading.set_visible(false);
    }

    fn build_categories(&self) {
        let remote = store::flathub_remote(Destination::User);
        let window = &self.window;
        for (key, title, icon, style) in [
            (
                "category/Graphics",
                "Create",
                "applications-graphics-symbolic",
                "create",
            ),
            (
                "category/Office",
                "Work",
                "x-office-document-symbolic",
                "work",
            ),
            (
                "category/Game",
                "Play",
                "applications-games-symbolic",
                "play",
            ),
            (
                "category/Social",
                "Socialize",
                "chat-message-new-symbolic",
                "socialize",
            ),
            (
                "category/Education",
                "Learn",
                "accessories-dictionary-symbolic",
                "learn",
            ),
            (
                "category/Development",
                "Develop",
                "applications-engineering-symbolic",
                "develop",
            ),
        ] {
            self.category_tiles.append(&store_category_tile(
                window, &remote, key, title, icon, style,
            ));
        }
        for (index, (key, label)) in [
            ("category/Office", "Productivity"),
            ("daily", "App of the Day"),
            ("category/Graphics", "Graphics & Photography"),
            ("category/AudioVideo", "Audio & Video"),
            ("mobile", "Apps for phones and tablets"),
            ("category/Education", "Education"),
            ("category/Network", "Networking"),
            ("category/Game", "Games and emulators"),
            ("category/Development", "Developer Tools"),
            ("category/Science", "Science"),
            ("category/System", "System"),
            ("category/Utility", "Utilities"),
            ("recently-updated", "New & Updated"),
        ]
        .into_iter()
        .enumerate()
        {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
            self.categories.append(&section);
            section.append(&home_label(tr(label), "title-2"));
            if key == "daily" {
                load_daily_feature(&section, window, &remote);
                continue;
            }
            if key == "category/Game" {
                section.add_css_class("packlift-games-banner");
                let games = gtk::Stack::new();
                games.set_vhomogeneous(false);
                let switcher = gtk::StackSwitcher::new();
                switcher.set_stack(Some(&games));
                switcher.set_halign(gtk::Align::Start);
                section.append(&switcher);
                section.append(&games);
                for (tab, (id, title, collection)) in [
                    ("games", "Games", key),
                    (
                        "emulators",
                        "Emulators",
                        "category/Game/subcategories?subcategory=Emulator",
                    ),
                    ("launchers", "Launchers", "game-launchers"),
                    (
                        "tools",
                        "Game tools",
                        "category/Game/subcategories?subcategory=GameTool",
                    ),
                ]
                .into_iter()
                .enumerate()
                {
                    let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
                    games.add_titled(&panel, Some(id), tr(title));
                    load_discovery_grid(
                        &panel,
                        window,
                        &remote,
                        &self.installed,
                        (collection, title),
                        index + tab,
                    );
                }
            } else {
                if key == "mobile" {
                    section.add_css_class("packlift-mobile-banner");
                }
                let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
                section.append(&panel);
                load_discovery_grid(
                    &panel,
                    window,
                    &remote,
                    &self.installed,
                    (key, label),
                    index,
                );
            }
        }
        self.categories.append(&other_categories(window));
    }

    fn prepare_banner(self: &Rc<Self>, apps: &[StoreChoice]) {
        let mut requested = self.banner_requested.borrow_mut();
        if requested.len() >= 6 && self.carousel.n_pages() > 0 {
            return;
        }
        for (remote, app) in apps
            .iter()
            .filter(|(_, app)| requested.insert(app.id.clone()))
            .take(6)
            .cloned()
        {
            let view = Rc::downgrade(self);
            background(
                move || {
                    let runner = worker().ok()?;
                    let details = store::app_details(&runner, &remote, &app).ok()?;
                    let (bytes, caption) =
                        details.screenshots.into_iter().take(2).find_map(|shot| {
                            store::screenshot_bytes(&runner, &shot.url)
                                .ok()
                                .map(|bytes| (bytes, shot.caption))
                        })?;
                    Some((remote, app, bytes, caption))
                },
                move |result| {
                    let (Some(view), Some((remote, app, bytes, caption))) =
                        (view.upgrade(), result)
                    else {
                        return;
                    };
                    let stream =
                        gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                    let Ok(image) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                        &stream,
                        1440,
                        1440,
                        true,
                        gio::Cancellable::NONE,
                    ) else {
                        return;
                    };
                    let banner = FeaturedApp {
                        remote,
                        app,
                        image: gtk::gdk::Texture::for_pixbuf(&image),
                        caption,
                    };
                    if !view.busy.get()
                        && view.page.get() == 1
                        && view
                            .catalog_search
                            .borrow()
                            .as_ref()
                            .is_some_and(CatalogSearch::is_explore)
                        && view.carousel.n_pages() < 4
                    {
                        let mut child = view.list.first_child();
                        while let Some(row) = child {
                            if row
                                .downcast_ref::<gtk::FlowBoxChild>()
                                .and_then(gtk::FlowBoxChild::child)
                                .is_some_and(|card| card.widget_name() == banner.app.id)
                            {
                                if view.carousel.n_pages() == 0 {
                                    view.list.remove(&row);
                                }
                                view.append_banner(&banner);
                                break;
                            }
                            child = row.next_sibling();
                        }
                    }
                    view.banners.borrow_mut().push(banner);
                },
            );
        }
    }

    fn append_banner(&self, banner: &FeaturedApp) {
        let color = self.page_colors.borrow()[&banner.app.id];
        self.carousel
            .append(&featured_app(&self.window, banner, color));
        self.feature.set_visible(true);
    }

    fn render_banner(&self, apps: &[StoreChoice]) -> Option<String> {
        let banners = self.banners.borrow();
        let mut first = None;
        for banner in banners
            .iter()
            .filter(|banner| apps.iter().any(|(_, app)| app.id == banner.app.id))
            .take(4)
        {
            first.get_or_insert_with(|| banner.app.id.clone());
            self.append_banner(banner);
        }
        first
    }
}

fn store_pagination() -> (gtk::Box, gtk::Button, gtk::Label, gtk::Button) {
    let nav = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    nav.set_halign(gtk::Align::Center);
    nav.add_css_class("packlift-store-pagination");
    let previous = gtk::Button::from_icon_name("go-previous-symbolic");
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    for (button, label) in [(&previous, tr("Previous")), (&next, tr("Next"))] {
        button.add_css_class("flat");
        button.set_tooltip_text(Some(label));
        button.update_property(&[gtk::accessible::Property::Label(label)]);
    }
    let page = home_label("", "dim-label");
    page.add_css_class("caption");
    page.set_halign(gtk::Align::Center);
    nav.append(&previous);
    nav.append(&page);
    nav.append(&next);
    (nav, previous, page, next)
}

fn discovery_grid() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .homogeneous(true)
        .row_spacing(10)
        .column_spacing(10)
        .build()
}

fn store_category_tile(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: &Remote,
    collection: &'static str,
    title: &'static str,
    icon: &str,
    style: &str,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("packlift-store-category");
    button.add_css_class(&format!("packlift-store-category-{style}"));
    button.set_hexpand(true);
    button.set_size_request(-1, 68);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_halign(gtk::Align::Center);
    let icon_well = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    icon_well.add_css_class("packlift-store-category-icon-well");
    icon_well.set_size_request(40, 40);
    icon_well.set_halign(gtk::Align::Center);
    icon_well.set_valign(gtk::Align::Center);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(23);
    image.set_hexpand(true);
    image.set_vexpand(true);
    image.add_css_class("packlift-store-category-icon");
    icon_well.append(&image);
    content.append(&icon_well);
    let label = gtk::Label::new(Some(tr(title)));
    label.add_css_class("heading");
    label.set_single_line_mode(true);
    content.append(&label);
    button.set_child(Some(&content));
    let (window, remote) = (window.clone(), remote.clone());
    button.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            show_collection(&window, &remote, collection, tr(title));
        }
    });
    button
}

fn load_discovery_grid(
    panel: &gtk::Box,
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: &Remote,
    installed: &Rc<RefCell<Option<BTreeSet<String>>>>,
    (collection, title): (&'static str, &'static str),
    color_offset: usize,
) {
    let (tile_owner, tile_remote, installed) = (window.clone(), remote.clone(), installed.clone());
    load_discovery_preview(
        panel,
        move || Ok(store::discovery_page(&worker()?, collection, 1)?.apps),
        move |apps| {
            let tiles = discovery_grid();
            let featured = collection == "mobile"
                || collection == "game-launchers"
                || collection.starts_with("category/Game");
            if featured {
                tiles.set_row_spacing(14);
                tiles.set_column_spacing(14);
            }
            let installed = installed.borrow();
            let is_installed =
                |app: &CatalogApp| installed.as_ref().is_some_and(|ids| ids.contains(&app.id));
            let (already_installed, mut recommended): (Vec<_>, Vec<_>) =
                apps.into_iter().partition(is_installed);
            recommended.extend(already_installed);
            if recommended.is_empty() {
                return None;
            }
            for (position, app) in recommended.into_iter().take(6).enumerate() {
                let color = (color_offset * 7 + position * 5) % APP_COLORS.len();
                tiles.append(&catalog_app_card(
                    &tile_owner,
                    tile_remote.clone(),
                    app,
                    color,
                    featured,
                ));
            }
            Some(tiles.upcast())
        },
    );
    let more = text_button(tr("More"));
    more.add_css_class("pill");
    more.set_halign(gtk::Align::Center);
    let accessible = tr("More in {category}").replace("{category}", tr(title));
    more.update_property(&[gtk::accessible::Property::Label(&accessible)]);
    let (window, remote) = (window.clone(), remote.clone());
    more.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            show_collection(&window, &remote, collection, tr(title));
        }
    });
    panel.append(&more);
}

fn load_discovery_preview<T: Send + 'static>(
    panel: &gtk::Box,
    fetch: impl Fn() -> crate::Result<T> + Send + Sync + 'static,
    render: impl Fn(T) -> Option<gtk::Widget> + 'static,
) {
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let status = home_label(tr("Loading apps…"), "dim-label");
    let retry = text_button(tr("Try again"));
    retry.set_halign(gtk::Align::Start);
    retry.set_visible(false);
    preview.append(&status);
    preview.append(&retry);
    panel.append(&preview);

    let fetch = Arc::new(fetch);
    let render = Rc::new(render);
    let busy = Rc::new(Cell::new(false));
    let settled = Rc::new(Cell::new(false));
    let load = Rc::new({
        let (preview, status, retry) = (preview.downgrade(), status.downgrade(), retry.downgrade());
        move || {
            let (Some(preview), Some(status), Some(retry)) =
                (preview.upgrade(), status.upgrade(), retry.upgrade())
            else {
                return;
            };
            if settled.get() || busy.replace(true) {
                return;
            }
            retry.set_visible(false);
            status.set_text(tr("Loading apps…"));
            let fetch = fetch.clone();
            let render = render.clone();
            let busy = busy.clone();
            let settled = settled.clone();
            let (preview, status, retry) =
                (preview.downgrade(), status.downgrade(), retry.downgrade());
            glib::MainContext::default().spawn_local(async move {
                let result = gio::spawn_blocking(move || fetch()).await;
                busy.set(false);
                let (Some(preview), Some(status), Some(retry)) =
                    (preview.upgrade(), status.upgrade(), retry.upgrade())
                else {
                    return;
                };
                if let Ok(Ok(data)) = result {
                    settled.set(true);
                    if let Some(content) = render(data) {
                        status.set_visible(false);
                        preview.append(&content);
                    } else {
                        status.set_text(tr("No apps found in this category."));
                    }
                } else {
                    status.set_text(tr("Could not load apps in this category."));
                    retry.set_visible(true);
                }
            });
        }
    });
    let load_on_map = load.clone();
    preview.connect_map(move |_| load_on_map());
    retry.connect_clicked(move |_| load());
}

fn other_categories(window: &glib::WeakRef<adw::ApplicationWindow>) -> gtk::Box {
    use store::AddonCategory;

    let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    section.append(&home_label(tr("Other categories"), "title-2"));
    let grid = discovery_grid();
    for (category, title, icon) in [
        (
            AddonCategory::Codecs,
            "Codecs",
            "multimedia-player-symbolic",
        ),
        (AddonCategory::Fonts, "Fonts", "font-x-generic-symbolic"),
        (
            AddonCategory::Drivers,
            "Hardware drivers",
            "video-display-symbolic",
        ),
        (
            AddonCategory::Input,
            "Input sources",
            "input-keyboard-symbolic",
        ),
        (
            AddonCategory::Languages,
            "Localization",
            "preferences-desktop-locale-symbolic",
        ),
    ] {
        let button = crate::navigation_button(tr(title), icon);
        button.add_css_class("packlift-store-card");
        button.set_hexpand(true);
        let owner = window.clone();
        button.connect_clicked(move |_| {
            if let Some(window) = owner.upgrade() {
                show_addons(&window, category, tr(title), icon);
            }
        });
        grid.append(&button);
    }
    section.append(&grid);
    section
}

fn show_addons(
    window: &adw::ApplicationWindow,
    category: store::AddonCategory,
    title: &str,
    icon: &'static str,
) {
    let (toolbar, header) = detail_page_toolbar(window, title, PageReturn::capture(window));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    hero.add_css_class("packlift-store-detail-hero");
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(24);
    image.add_css_class("packlift-scope-icon");
    image.set_valign(gtk::Align::Center);
    hero.append(&image);
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
    heading.append(&home_label(title, "title-2"));
    let introduction = home_label(
        tr("Flatpak add-ons from your configured repositories."),
        "dim-label",
    );
    introduction.set_max_width_chars(76);
    heading.append(&introduction);
    hero.append(&heading);
    body.append(&hero);
    let loading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    loading.add_css_class("packlift-addon-loading");
    loading.set_margin_top(8);
    loading.set_margin_bottom(8);
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(24, 24);
    spinner.start();
    loading.append(&spinner);
    let loading_message = home_label(
        tr("Checking configured repositories. This may take a little while."),
        "dim-label",
    );
    loading_message.set_max_width_chars(68);
    loading_message.set_valign(gtk::Align::Center);
    loading.append(&loading_message);
    body.append(&loading);
    let status = home_label("", "dim-label");
    status.set_visible(false);
    status.set_max_width_chars(76);
    body.append(&status);
    let region = gtk::ListBox::new();
    region.set_selection_mode(gtk::SelectionMode::None);
    region.add_css_class("boxed-list");
    body.append(&region);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&adw::Clamp::builder().maximum_size(920).child(&body).build())
        .build();
    toolbar.set_content(Some(&scroll));
    crate::updates_ui::attach(&toolbar, &header, window);
    window.set_widget_name("packlift-store-addons");
    window.set_title(Some(title));
    window.set_content(Some(&toolbar));

    let (owner, status, region, loading, spinner) = (
        window.downgrade(),
        status.downgrade(),
        region.downgrade(),
        loading.downgrade(),
        spinner.downgrade(),
    );
    background(
        move || worker().and_then(|runner| store::addons(&runner, category)),
        move |result| {
            let (Some(status), Some(region), Some(loading), Some(spinner)) = (
                status.upgrade(),
                region.upgrade(),
                loading.upgrade(),
                spinner.upgrade(),
            ) else {
                return;
            };
            spinner.stop();
            loading.set_visible(false);
            match result {
                Ok((addons, partial)) => {
                    if partial {
                        status.set_text(tr(
                            "Some repositories could not be reached; showing available add-ons.",
                        ));
                        status.set_visible(true);
                    } else if addons.is_empty() {
                        status.set_text(tr("No Flatpak add-ons found in this category."));
                        status.set_visible(true);
                    } else {
                        status.set_text(tr("Preparing the add-on list…"));
                        status.set_visible(true);
                    }
                    if addons.is_empty() {
                        return;
                    }
                    let mut grouped = std::collections::BTreeMap::new();
                    for addon in addons {
                        grouped
                            .entry((
                                addon.name.clone(),
                                addon.reference.clone(),
                                addon.remote.name.clone(),
                                addon.remote.url.clone(),
                            ))
                            .or_insert_with(Vec::new)
                            .push(addon);
                    }
                    let mut addons = grouped.into_values();
                    let owner = owner.clone();
                    let status = status.downgrade();
                    let region = region.downgrade();
                    glib::idle_add_local(move || {
                        let (Some(status), Some(region)) = (status.upgrade(), region.upgrade())
                        else {
                            return glib::ControlFlow::Break;
                        };
                        for _ in 0..16 {
                            let Some(addon_group) = addons.next() else {
                                status.set_visible(partial);
                                if partial {
                                    status.set_text(tr(
                                        "Some repositories could not be reached; showing available add-ons.",
                                    ));
                                }
                                return glib::ControlFlow::Break;
                            };
                            region.append(&addon_row(&owner, icon, addon_group));
                        }
                        glib::ControlFlow::Continue
                    });
                }
                Err(error) => {
                    status.set_text(&error.to_string());
                    status.set_visible(true);
                }
            }
        },
    );
}

fn addon_row(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    icon: &str,
    mut choices: Vec<store::Addon>,
) -> gtk::ListBoxRow {
    choices.sort_by_key(|addon| addon.remote.destination == Destination::System);
    let first = &choices[0];
    let scope_summary = choices
        .iter()
        .map(|addon| {
            let scope = match addon.remote.destination {
                Destination::User => tr("Account"),
                Destination::System => tr("System"),
            };
            let state = if addon.installed {
                tr("Installed")
            } else {
                tr("Available")
            };
            format!("{scope}: {state}")
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .tooltip_text(format!("{scope_summary}\n{}", first.reference))
        .build();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.add_css_class("packlift-addon-row");
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(18);
    image.add_css_class("packlift-scope-icon");
    image.set_valign(gtk::Align::Center);
    content.append(&image);

    let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    labels.append(&addon_label(&first.name, "heading"));
    if !first.description.is_empty() {
        labels.append(&addon_label(&first.description, "dim-label"));
    }
    let mut reference = first.reference.split('/');
    let _ = reference.next(); // Flatpak reference kind, always "runtime" here.
    let id = reference.next().unwrap_or(&first.reference);
    let arch = reference.next().unwrap_or_default();
    let branch = reference.next().unwrap_or_default();
    labels.append(&addon_label(id, "packlift-addon-metadata"));
    let context = format!("{} · {branch} · {arch}", first.remote.label());
    let repository = addon_label(&context, "packlift-addon-metadata");
    repository.set_tooltip_text(Some(&format!("{context}\n{scope_summary}")));
    labels.append(&repository);
    content.append(&labels);

    let pending: Vec<_> = choices
        .into_iter()
        .filter(|addon| !addon.installed)
        .collect();
    if pending.is_empty() {
        let installed = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        installed.add_css_class("packlift-addon-installed");
        installed.set_valign(gtk::Align::Center);
        installed.append(&gtk::Image::from_icon_name("emblem-ok-symbolic"));
        installed.append(&gtk::Label::new(Some(tr("Installed"))));
        content.append(&installed);
    } else if let [addon] = pending.as_slice() {
        let install = addon_install_button(tr("Install"));
        install.set_tooltip_text(Some(match addon.remote.destination {
            Destination::User => tr("Install for my account"),
            Destination::System => tr("Install for the system"),
        }));
        queue_addon_install(window, &install, addon.clone());
        content.append(&install);
    } else {
        let install = gtk::MenuButton::builder()
            .label(tr("Install"))
            .valign(gtk::Align::Center)
            .width_request(100)
            .build();
        let popover = gtk::Popover::new();
        let options = gtk::Box::new(gtk::Orientation::Vertical, 6);
        options.set_margin_top(8);
        options.set_margin_bottom(8);
        options.set_margin_start(8);
        options.set_margin_end(8);
        for addon in pending {
            let label = match addon.remote.destination {
                Destination::User => tr("Install for my account"),
                Destination::System => tr("Install for the system"),
            };
            let action = addon_install_button(label);
            action.set_halign(gtk::Align::Fill);
            queue_addon_install(window, &action, addon);
            options.append(&action);
        }
        popover.set_child(Some(&options));
        install.set_popover(Some(&popover));
        content.append(&install);
    }
    row.set_child(Some(&content));
    row
}

fn addon_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .tooltip_text(text)
        .build();
    label.add_css_class(class);
    label
}

fn addon_install_button(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .width_request(100)
        .build()
}

fn queue_addon_install(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    button: &gtk::Button,
    addon: store::Addon,
) {
    let owner = window.clone();
    let title = format!(
        "{} · {} · {} · {}",
        tr("Installing Flatpak add-on"),
        addon.name,
        addon.reference,
        match addon.remote.destination {
            Destination::User => tr("Account"),
            Destination::System => tr("System"),
        }
    );
    button.connect_clicked(move |button| {
        let Some(app) = owner.upgrade().and_then(|window| window.application()) else {
            return;
        };
        if crate::operations_ui::has_active_title(&title) {
            return;
        }
        let original_label = button.label().unwrap_or_default();
        button.set_sensitive(false);
        button.set_label(tr("Queued"));
        let (completed, released) = (button.downgrade(), button.downgrade());
        let addon = addon.clone();
        crate::operations_ui::submit(
            &app,
            &title,
            move |log| store::install_addon(&Runner::new(None, log)?, &addon).map(|_| None),
            move |result| {
                if result.is_ok() {
                    if let Some(button) = completed.upgrade() {
                        button.set_label(tr("Installed"));
                    }
                }
            },
            move || {
                if let Some(button) = released
                    .upgrade()
                    .filter(|button| button.label().as_deref() == Some(tr("Queued")))
                {
                    button.set_label(&original_label);
                    button.set_sensitive(true);
                }
            },
        );
    });
}

#[cfg(test)]
mod addon_tests {
    use super::*;

    #[test]
    fn addon_rows_keep_actions_on_one_line_and_identify_runtime_variants() {
        crate::with_gtk_test(|| {
            let addon = store::Addon {
                remote: store::flathub_remote(Destination::User),
                reference: "runtime/org.freedesktop.Platform.codecs-extra/x86_64/25.08-extra"
                    .into(),
                name: "Codecs Extra Extension".into(),
                description: "This extension contains extra codecs for the Flatpak platform."
                    .into(),
                installed: true,
            };
            let mut system = addon.clone();
            system.remote.destination = Destination::System;
            let window = glib::WeakRef::new();
            let icon = "multimedia-player-symbolic";

            let installed_row = addon_row(&window, icon, vec![addon.clone(), system.clone()]);
            let content = installed_row.child().and_downcast::<gtk::Box>().unwrap();
            let badge = content.last_child().and_downcast::<gtk::Box>().unwrap();
            assert!(badge.has_css_class("packlift-addon-installed"));
            let label = badge.last_child().and_downcast::<gtk::Label>().unwrap();
            assert_eq!(label.text(), tr("Installed"));
            assert!(!label.wraps());
            let labels = badge.prev_sibling().and_downcast::<gtk::Box>().unwrap();
            let metadata = labels.last_child().and_downcast::<gtk::Label>().unwrap();
            assert_eq!(metadata.text(), "Flathub · 25.08-extra · x86_64");
            let id = metadata
                .prev_sibling()
                .and_downcast::<gtk::Label>()
                .unwrap();
            assert_eq!(id.text(), "org.freedesktop.Platform.codecs-extra");
            assert_eq!(id.ellipsize(), gtk::pango::EllipsizeMode::End);
            assert!(id.is_single_line_mode());
            assert!(installed_row.measure(gtk::Orientation::Horizontal, -1).0 <= 320);

            system.installed = false;
            let install_row = addon_row(&window, icon, vec![addon.clone(), system.clone()]);
            let content = install_row.child().and_downcast::<gtk::Box>().unwrap();
            let button = content.last_child().and_downcast::<gtk::Button>().unwrap();
            let label = button.child().and_downcast::<gtk::Label>().unwrap();
            assert_eq!(label.text(), tr("Install"));
            assert!(!label.wraps());
            assert_eq!(
                button.tooltip_text().as_deref(),
                Some(tr("Install for the system"))
            );
            assert!(install_row.measure(gtk::Orientation::Horizontal, -1).0 <= 320);

            let mut user = addon;
            user.installed = false;
            let menu_row = addon_row(&window, icon, vec![user, system]);
            let content = menu_row.child().and_downcast::<gtk::Box>().unwrap();
            let menu = content
                .last_child()
                .and_downcast::<gtk::MenuButton>()
                .unwrap();
            assert_eq!(menu.label().as_deref(), Some(tr("Install")));
            let options = menu
                .popover()
                .unwrap()
                .child()
                .and_downcast::<gtk::Box>()
                .unwrap();
            let mut action = options.first_child();
            for expected in [tr("Install for my account"), tr("Install for the system")] {
                let button = action.and_downcast::<gtk::Button>().unwrap();
                let label = button.child().and_downcast::<gtk::Label>().unwrap();
                assert_eq!(label.text(), expected);
                assert!(!label.wraps());
                action = button.next_sibling();
            }
            assert!(action.is_none());
            assert!(menu_row.measure(gtk::Orientation::Horizontal, -1).0 <= 320);
        });
    }
}

fn show_collection(
    window: &adw::ApplicationWindow,
    remote: &Remote,
    collection: &'static str,
    title: &'static str,
) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, "")));
    back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);

    let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
    section.set_margin_top(18);
    section.set_margin_bottom(18);
    section.set_margin_start(18);
    section.set_margin_end(18);
    let tiles = discovery_grid();
    let status = home_label("", "dim-label");
    section.append(&status);
    let retry = text_button(tr("Try again"));
    retry.set_halign(gtk::Align::Start);
    retry.set_visible(false);
    section.append(&retry);
    section.append(&tiles);
    let clamp = adw::Clamp::builder()
        .maximum_size(1180)
        .child(&section)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&clamp)
        .build();
    toolbar.set_content(Some(&scroll));
    crate::updates_ui::attach(&toolbar, &header, window);

    let page = Rc::new(Cell::new(1usize));
    let has_next = Rc::new(Cell::new(true));
    let busy = Rc::new(Cell::new(false));
    let failed_page = Rc::new(Cell::new(None));
    let load = Rc::new({
        let (tiles, status, retry) = (tiles.downgrade(), status.downgrade(), retry.downgrade());
        let window = window.downgrade();
        let remote = remote.clone();
        let page = page.clone();
        let has_next = has_next.clone();
        let busy = busy.clone();
        let failed_page = failed_page.clone();
        move |number: usize| {
            if busy.get() {
                return;
            }
            if number > 1 && !has_next.get() {
                return;
            }
            let (Some(tiles), Some(status), Some(retry)) =
                (tiles.upgrade(), status.upgrade(), retry.upgrade())
            else {
                return;
            };
            busy.set(true);
            failed_page.set(None);
            retry.set_visible(false);
            status.set_visible(true);
            status.set_text(tr("Loading apps…"));
            let failed_page = failed_page.clone();
            let (window, remote, page, has_next, busy) = (
                window.clone(),
                remote.clone(),
                page.clone(),
                has_next.clone(),
                busy.clone(),
            );
            background(
                move || {
                    worker().and_then(|runner| store::discovery_page(&runner, collection, number))
                },
                move |result| {
                    busy.set(false);
                    match result {
                        Ok(result) => {
                            page.set(number);
                            has_next.set(result.has_next);
                            if number == 1 {
                                tiles.remove_all();
                            }
                            for (position, app) in result.apps.into_iter().enumerate() {
                                let color = (number + position * 5) % APP_COLORS.len();
                                tiles.append(&app_card(&window, remote.clone(), app, color));
                            }
                            let empty = tiles.first_child().is_none();
                            status.set_visible(empty);
                            if empty {
                                status.set_text(tr("No apps match this search."));
                            }
                        }
                        Err(error) => {
                            status.set_text(tr("Could not load apps in this category."));
                            status.set_tooltip_text(Some(&error.to_string()));
                            failed_page.set(Some(number));
                            retry.set_visible(true);
                        }
                    }
                },
            );
        }
    });

    let retry_load = load.clone();
    let retry_page = failed_page.clone();
    retry.connect_clicked(move |_| {
        if let Some(number) = retry_page.get() {
            retry_load(number);
        }
    });
    let vadjustment = scroll.vadjustment();
    let load_more = load.clone();
    let page_for_scroll = page.clone();
    vadjustment.connect_value_changed(move |adj| {
        if failed_page.get().is_none()
            && adj.upper() > adj.page_size()
            && adj.value() + adj.page_size() >= adj.upper() - 240.0
        {
            load_more(page_for_scroll.get() + 1);
        }
    });
    window.set_widget_name("packlift-store-collection");
    window.set_title(Some(title));
    window.set_content(Some(&toolbar));
    load(1);
}

fn load_daily_feature(
    section: &gtk::Box,
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: &Remote,
) {
    let (window, remote) = (window.clone(), remote.clone());
    let fetch_remote = remote.clone();
    load_discovery_preview(
        section,
        move || {
            let runner = worker()?;
            let mut apps = store::discovery_page(&runner, "trending", 1)?.apps;
            if apps.is_empty() {
                return Ok(None);
            }
            let days = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                / 86400;
            let index = days as usize % apps.len();
            let app = apps.swap_remove(index);
            let preview = store::app_details(&runner, &fetch_remote, &app)
                .ok()
                .and_then(|details| {
                    let shot = details.screenshots.into_iter().next()?;
                    let bytes = store::screenshot_bytes(&runner, &shot.url).ok()?;
                    Some((bytes, shot.caption))
                });
            Ok(Some((app, preview)))
        },
        move |result| {
            let (app, preview) = result?;
            let color = app_color(&app.id);
            if let Some((bytes, caption)) = preview {
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                    &stream,
                    1440,
                    1440,
                    true,
                    gio::Cancellable::NONE,
                ) {
                    let featured = FeaturedApp {
                        remote: remote.clone(),
                        app,
                        image: gtk::gdk::Texture::for_pixbuf(&pixbuf),
                        caption,
                    };
                    return Some(featured_app(&window, &featured, color).upcast());
                }
            }
            Some(app_card(&window, remote.clone(), app, color).upcast())
        },
    );
}

fn store_carousel() -> (gtk::Box, adw::Carousel) {
    let feature = gtk::Box::new(gtk::Orientation::Vertical, 4);
    feature.set_visible(false);
    let carousel = adw::Carousel::builder()
        .allow_scroll_wheel(false)
        .reveal_duration(0)
        .hexpand(true)
        .build();
    feature.append(&carousel);

    let wheel = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
    );
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    let slides = carousel.downgrade();
    wheel.connect_scroll(move |_, dx, dy| {
        let Some(carousel) = slides.upgrade().filter(|carousel| carousel.n_pages() > 1) else {
            return glib::Propagation::Proceed;
        };
        let delta = if dy.abs() >= dx.abs() { dy } else { dx };
        if delta == 0.0 {
            return glib::Propagation::Proceed;
        }
        advance_banner(&carousel, if delta > 0.0 { 1 } else { -1 });
        glib::Propagation::Stop
    });
    carousel.add_controller(wheel);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_halign(gtk::Align::Center);
    controls.set_visible(false);
    let indicator = adw::CarouselIndicatorDots::builder()
        .carousel(&carousel)
        .build();
    for (step, icon, label) in [
        (-1, "go-previous-symbolic", tr("Previous")),
        (1, "go-next-symbolic", tr("Next")),
    ] {
        if step == 1 {
            controls.append(&indicator);
        }
        let button = gtk::Button::from_icon_name(icon);
        button.add_css_class("flat");
        button.set_tooltip_text(Some(label));
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        let slides = carousel.downgrade();
        button.connect_clicked(move |_| {
            if let Some(carousel) = slides.upgrade() {
                advance_banner(&carousel, step);
            }
        });
        controls.append(&button);
    }
    let nav = controls.downgrade();
    carousel.connect_n_pages_notify(move |carousel| {
        if let Some(nav) = nav.upgrade() {
            nav.set_visible(carousel.n_pages() > 1);
        }
    });
    feature.append(&controls);

    let pointer = gtk::EventControllerMotion::new();
    feature.add_controller(pointer.clone());
    let focus = gtk::EventControllerFocus::new();
    feature.add_controller(focus.clone());
    let interval = std::time::Duration::from_secs(6);
    let next_rotation = Cell::new(std::time::Instant::now() + interval);
    let slides = carousel.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        let Some(carousel) = slides.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let now = std::time::Instant::now();
        if !carousel.is_mapped() || pointer.contains_pointer() || focus.contains_focus() {
            next_rotation.set(now + interval);
        } else if now >= next_rotation.get() {
            advance_banner(&carousel, 1);
            next_rotation.set(now + interval);
        }
        glib::ControlFlow::Continue
    });
    (feature, carousel)
}

fn advance_banner(carousel: &adw::Carousel, step: i64) {
    let count = i64::from(carousel.n_pages());
    if count > 1 {
        let current = carousel.position().round() as i64;
        let next = (current + step).rem_euclid(count) as u32;
        carousel.scroll_to(&carousel.nth_page(next), true);
    }
}

fn show_app(window: &adw::ApplicationWindow, remote: &Remote, app: &CatalogApp, color: usize) {
    let previous_page = PageReturn::capture(window);
    let key = format!(
        "{}:{}:{}",
        remote.destination.flag(),
        remote.url,
        app.reference
    );
    if let Some((details, facts)) = DETAIL_CACHE.with_borrow(|cache| {
        cache
            .iter()
            .find(|(id, time, _, _)| id == &key && time.elapsed() < Duration::from_secs(300))
            .map(|(_, _, details, facts)| (details.clone(), facts.clone()))
    }) {
        show_app_details(
            window,
            remote,
            app,
            color,
            Ok(details),
            facts,
            previous_page,
        );
        return;
    }
    window.set_widget_name("packlift-store-loading");
    let (toolbar, header) = detail_page_toolbar(window, &app.name, previous_page.clone());
    let previous = previous_page;
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_halign(gtk::Align::Center);
    body.set_valign(gtk::Align::Center);
    body.set_hexpand(true);
    body.set_vexpand(true);
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(36, 36);
    spinner.start();
    spinner.set_halign(gtk::Align::Center);
    body.append(&spinner);
    let title = home_label(&app.name, "title-3");
    title.set_halign(gtk::Align::Center);
    body.append(&title);
    let status = home_label(tr("Loading app information…"), "dim-label");
    status.set_halign(gtk::Align::Center);
    body.append(&status);
    toolbar.set_content(Some(&body));
    crate::updates_ui::attach(&toolbar, &header, window);
    window.set_title(Some(&app.name));
    window.set_content(Some(&toolbar));
    window.present();

    let loading = toolbar.upcast::<gtk::Widget>().downgrade();
    let window = window.downgrade();
    let remote = remote.clone();
    let app = app.clone();
    let (detail_remote, detail_app) = (remote.clone(), app.clone());
    background(
        move || match worker() {
            Ok(runner) => std::thread::scope(|tasks| {
                let facts = detail_remote
                    .flathub
                    .then(|| tasks.spawn(|| store::flathub_facts(&runner, &detail_app)));
                let details = store::app_details(&runner, &detail_remote, &detail_app);
                (
                    details,
                    facts.and_then(|task| task.join().ok()).unwrap_or_default(),
                )
            }),
            Err(error) => (Err(error), store::FlathubFacts::default()),
        },
        move |(details, facts)| {
            if let Ok(details) = &details {
                DETAIL_CACHE.with_borrow_mut(|cache| {
                    cache.retain(|(id, _, _, _)| *id != key);
                    if cache.len() >= 48 {
                        cache.pop_front();
                    }
                    cache.push_back((key, Instant::now(), details.clone(), facts.clone()));
                });
            }
            let (Some(window), Some(loading)) = (window.upgrade(), loading.upgrade()) else {
                return;
            };
            if window.content().as_ref() == Some(&loading) {
                show_app_details(&window, &remote, &app, color, details, facts, previous);
            }
        },
    );
}

fn detail_page_toolbar(
    window: &adw::ApplicationWindow,
    title: &str,
    previous: Option<PageReturn>,
) -> (adw::ToolbarView, adw::HeaderBar) {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, "")));
    if let Some(previous) = previous {
        let owner = window.downgrade();
        page_navigation(
            &toolbar,
            &header,
            tr("Back"),
            "go-previous-symbolic",
            move || {
                if let Some(window) = owner.upgrade() {
                    previous.restore(&window);
                }
            },
        );
    }
    toolbar.add_top_bar(&header);
    (toolbar, header)
}

fn show_app_details(
    window: &adw::ApplicationWindow,
    remote: &Remote,
    app: &CatalogApp,
    color: usize,
    details_result: std::result::Result<store::AppDetails, packlift::core::PackliftError>,
    extra: store::FlathubFacts,
    previous: Option<PageReturn>,
) {
    window.set_widget_name("packlift-store-details");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 24);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(18);
    body.set_margin_bottom(24);

    let hero = gtk::Box::new(gtk::Orientation::Vertical, 16);
    hero.add_css_class("packlift-store-detail-hero");
    let identity = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    identity.append(&catalog_icon_frame(app, 76, 88));
    let title = gtk::Box::new(gtk::Orientation::Vertical, 4);
    title.set_hexpand(true);
    title.set_valign(gtk::Align::Center);
    title.append(&home_label(&app.name, "title-1"));
    if !app.summary.is_empty() && app.summary != app.id {
        title.append(&home_label(&app.summary, "dim-label"));
    }
    let developer = home_label("", "dim-label");
    developer.set_size_request(-1, 20);
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
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 6);
    actions.set_hexpand(false);
    actions.set_valign(gtk::Align::Center);
    actions.append(&install_controls);
    if remote.flathub {
        actions.append(&favorite_button(app, window));
    }
    identity.append(&actions);

    let installed_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    installed_actions.set_visible(false);
    let open = text_button(tr("Open"));
    open.add_css_class("suggested-action");
    open.add_css_class("pill");
    installed_actions.append(&open);

    let uninstall = gtk::MenuButton::builder()
        .label(tr("Uninstall"))
        .direction(gtk::ArrowType::Down)
        .build();
    uninstall.set_valign(gtk::Align::Center);
    uninstall.add_css_class("destructive-action");
    uninstall.add_css_class("pill");
    installed_actions.append(&uninstall);
    actions.append(&installed_actions);
    hero.append(&identity);
    let progress = gtk::Box::new(gtk::Orientation::Vertical, 0);
    progress.set_visible(false);
    progress.set_width_request(128);
    actions.append(&progress);

    let facts = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(4)
        .homogeneous(true)
        .column_spacing(1)
        .row_spacing(1)
        .build();
    facts.set_vexpand(false);
    facts.set_visible(false);
    facts.add_css_class("packlift-store-facts");
    facts.set_overflow(gtk::Overflow::Hidden);
    body.append(&hero);

    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let gallery_status = home_label(tr("Loading screenshots…"), "dim-label");
    gallery_status.set_valign(gtk::Align::Center);
    gallery.append(&gallery_status);
    body.append(&gallery);

    let about = gtk::Box::new(gtk::Orientation::Vertical, 8);
    about.append(&home_label(tr("About this app"), "title-3"));
    let description = home_label("", "");
    description.set_selectable(true);
    description.set_visible(false);
    about.append(&description);
    let app_id = home_label(&app.id, "dim-label");
    app_id.set_selectable(true);
    about.append(&app_id);
    body.append(&about);
    body.append(&facts);

    let releases = gtk::Box::new(gtk::Orientation::Vertical, 12);
    releases.set_visible(false);
    body.append(&releases);
    let (sources_section, sources) = detail_list(tr("Sources"));
    body.append(&sources_section);
    let project = gtk::Box::new(gtk::Orientation::Vertical, 12);
    project.set_visible(false);
    body.append(&project);
    let reviews = gtk::Box::new(gtk::Orientation::Vertical, 10);
    reviews.set_visible(remote.flathub);
    body.append(&reviews);
    let similar_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    similar_section.set_visible(false);
    similar_section.append(&home_label(tr("Suggested apps"), "title-3"));
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
        .maximum_size(900)
        .tightening_threshold(790)
        .child(&body)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .hexpand(true)
        .child(&layout)
        .build();
    let (toolbar, header) = detail_page_toolbar(window, &app.name, previous);
    toolbar.set_content(Some(&scroll));
    crate::updates_ui::attach(&toolbar, &header, window);
    window.set_widget_name("packlift-store-details");
    window.set_title(Some(&app.name));
    let installed = Rc::new(RefCell::new(Vec::new()));
    let launch_state = installed.clone();
    let application = window.application().map(|app| app.downgrade());
    open.connect_clicked(move |_| {
        let records = launch_state.borrow();
        let record = records
            .iter()
            .find(|record: &&Value| record["flatpak_scope"] == "user")
            .or_else(|| records.first());
        if let (Some(record), Some(app)) = (
            record,
            application.as_ref().and_then(glib::WeakRef::upgrade),
        ) {
            if let (Some(scope), Some(target)) =
                (record["flatpak_scope"].as_str(), record["target"].as_str())
            {
                app.activate_action(
                    "open-installed-flatpak",
                    Some(&(scope, target).to_variant()),
                );
            }
        }
    });
    let remove = uninstall.downgrade();
    let owner = window.downgrade();
    let job = progress.downgrade();
    let name = app.name.clone();
    let installed_state = installed.clone();
    let controls = install_controls.downgrade();
    let refresh_button = installed_actions.downgrade();
    let open_button = open.downgrade();
    let app_id = app.id.clone();
    let refresh_installation = move || {
        let id = app_id.clone();
        let open_button = open_button.clone();
        let (remove, owner, job, name) = (remove.clone(), owner.clone(), job.clone(), name.clone());
        let state = installed_state.clone();
        let (controls, refresh_button) = (controls.clone(), refresh_button.clone());
        let (Some(install_controls), Some(button)) = (controls.upgrade(), refresh_button.upgrade())
        else {
            return;
        };
        install_controls.set_sensitive(false);
        button.set_sensitive(false);
        background(
            move || crate::installer()?.flatpak_app_installations(&id),
            move |result| {
                let (Some(controls), Some(button)) = (controls.upgrade(), refresh_button.upgrade())
                else {
                    return;
                };
                if let Ok(records) = result {
                    let is_installed = !records.is_empty();
                    if let Some(remove) = remove.upgrade() {
                        remove.set_popover(
                            is_installed
                                .then(|| {
                                    uninstall_scope_popover(&owner, &job, &name, records.clone())
                                })
                                .as_ref(),
                        );
                    }
                    *state.borrow_mut() = records;
                    controls.set_visible(!is_installed);
                    button.set_visible(is_installed);
                }
                controls.set_sensitive(true);
                button.set_sensitive(true);
                if button.is_visible() && button.root().is_some_and(|root| root.focus().is_none()) {
                    if let Some(open) = open_button.upgrade() {
                        open.grab_focus();
                    }
                }
            },
        );
    };
    let on_map = refresh_installation.clone();
    let job = progress.downgrade();
    toolbar.connect_map(move |_| {
        if job.upgrade().is_some_and(|job| !job.is_visible()) {
            on_map();
        }
    });
    let (controls, installed_group, install_button) = (
        install_controls.downgrade(),
        installed_actions.downgrade(),
        install.downgrade(),
    );
    progress.connect_visible_notify(move |progress| {
        if let Some(install) = install_button.upgrade() {
            install.set_visible(!progress.is_visible());
        }
        if progress.is_visible() {
            if let (Some(controls), Some(installed)) =
                (controls.upgrade(), installed_group.upgrade())
            {
                controls.set_sensitive(false);
                installed.set_visible(false);
            }
        } else {
            refresh_installation();
        }
    });
    let preferred = remote.clone();
    let parent = window.downgrade();
    let job = progress.downgrade();
    let install_app = move |source: Remote, app: CatalogApp| {
        if let (Some(window), Some(progress)) = (parent.upgrade(), job.upgrade()) {
            install_store_app(&window, &progress, source, app);
        }
    };
    let (repository, install) = (repository.downgrade(), install.downgrade());
    load_detail_sources(remote, app, sources_section, sources, move |choices| {
        if let (Some(repository), Some(install)) = (repository.upgrade(), install.upgrade()) {
            bind_install_menu(&repository, &install, &preferred, choices, install_app);
        }
    });
    if let Some(size) = extra.download_size {
        facts.append(&store_fact(
            "folder-download-symbolic",
            tr("Download size"),
            &glib::format_size(size),
            tr("Additional runtimes may need to be downloaded."),
        ));
    }
    if let Some(groups) = extra.permissions {
        facts.append(&permissions_fact(groups));
    }
    let catalog_source = remote.clone();
    let id = app.id.clone();
    let parent = window.downgrade();
    let category = details_result
        .as_ref()
        .ok()
        .and_then(|details| details.category.clone());
    let (parent_for_cards, id) = (parent.clone(), id);
    background(
        move || {
            worker()
                .map(|runner| {
                    store::suggested_apps(&runner, &catalog_source, &id, category.as_deref())
                })
                .unwrap_or_default()
        },
        move |apps| {
            similar_section.set_visible(!apps.is_empty());
            for (index, (remote, app)) in apps.into_iter().enumerate() {
                similar.append(&app_card(
                    &parent_for_cards,
                    remote,
                    app,
                    (color + index + 1) % APP_COLORS.len(),
                ));
            }
        },
    );
    let review_version = details_result
        .as_ref()
        .ok()
        .and_then(|details| details.releases.first())
        .map_or("unknown", |release| release.version.as_str())
        .to_owned();
    match details_result {
        Ok(details) => {
            developer.set_text(&details.developer);
            developer.set_visible(!details.developer.is_empty());
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
                        tr("Works on phones, tablets and desktops.")
                    } else {
                        tr("Designed primarily for desktops.")
                    },
                ));
                facts.set_visible(true);
            }
            if let Some(age) = details.age {
                facts.append(&store_fact(
                    "avatar-default-symbolic",
                    tr("Age rating"),
                    &format!("{age}+"),
                    tr("Minimum recommended age."),
                ));
                facts.set_visible(true);
            }
            if !details.description.is_empty() {
                description.set_text(&details.description);
                description.set_visible(true);
            }
            show_release_history(&releases, details.releases);
            show_project_details(
                &project,
                details.license,
                details.free_software,
                extra.installs_last_month,
                details.links,
                &parent,
            );
            if details.screenshots.is_empty() {
                gallery_status.set_text(tr("No screenshots are available from this repository."));
            } else {
                gallery.remove(&gallery_status);
                bind_gallery(&gallery, details.screenshots);
            }
        }
        Err(_) => gallery_status.set_text(tr("App information could not be loaded.")),
    }
    facts.set_visible(facts.first_child().is_some());
    if remote.flathub {
        show_reviews(&reviews, &app.id, review_version);
    }
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
    let group = gtk::ListBox::new();
    group.set_selection_mode(gtk::SelectionMode::None);
    group.add_css_class("boxed-list");
    let latest = gtk::ListBoxRow::builder()
        .activatable(false)
        .child(&release_card(latest, true))
        .build();
    group.append(&latest);
    let older = releases.collect::<Vec<_>>();
    if !older.is_empty() {
        let history = adw::ExpanderRow::builder()
            .title(tr("Version history"))
            .build();
        for release in older {
            history.add_row(&release_card(release, false));
        }
        group.append(&history);
    }
    container.append(&group);
    container.set_visible(true);
}

fn release_card(release: store::Release, preview: bool) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("packlift-store-release");
    let version = tr("Version {version}").replace("{version}", &release.version);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = home_label(&version, "heading");
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
        if preview
            && (release.notes.chars().nth(220).is_some() || release.notes.lines().count() > 4)
        {
            let limit_notes = |notes: &gtk::Label, limited| {
                notes.set_lines(if limited { 4 } else { -1 });
                notes.set_ellipsize(if limited {
                    gtk::pango::EllipsizeMode::End
                } else {
                    gtk::pango::EllipsizeMode::None
                });
            };
            limit_notes(&notes, true);
            let toggle = gtk::ToggleButton::with_label(tr("Show more"));
            toggle.add_css_class("pill");
            toggle.set_halign(gtk::Align::Center);
            toggle.connect_toggled(move |toggle| {
                let expanded = toggle.is_active();
                limit_notes(&notes, !expanded);
                toggle.set_label(if expanded {
                    tr("Show less")
                } else {
                    tr("Show more")
                });
            });
            card.append(&toggle);
        }
    }
    card
}

fn show_project_details(
    section: &gtk::Box,
    license: String,
    free_software: Option<bool>,
    installs: Option<u64>,
    links: Vec<(&'static str, String)>,
    parent: &glib::WeakRef<adw::ApplicationWindow>,
) {
    if !license.is_empty() || installs.is_some() {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        card.set_valign(gtk::Align::Start);
        card.add_css_class("packlift-store-info-card");
        let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
        icon.set_halign(gtk::Align::Start);
        icon.add_css_class("packlift-store-fact-badge");
        card.append(&icon);
        if !license.is_empty() {
            let label = match free_software {
                Some(true) => tr("Free software"),
                Some(false) => tr("Proprietary software"),
                None => tr("License"),
            };
            card.append(&home_label(label, "heading"));
            let value = home_label(&license, "dim-label");
            value.set_selectable(true);
            card.append(&value);
        }
        if let Some(installs) = installs {
            card.append(&home_label(
                &format!("{} · {installs}", tr("Installs last month")),
                "dim-label",
            ));
        }
        section.append(&card);
    }
    if !links.is_empty() {
        let rows = gtk::ListBox::new();
        rows.set_valign(gtk::Align::Start);
        rows.set_selection_mode(gtk::SelectionMode::None);
        rows.add_css_class("boxed-list");
        for (name, url) in links {
            let icon = match name {
                "Donation" | "Donate" => "emblem-favorite-symbolic",
                "Translate" => "preferences-desktop-locale-symbolic",
                "Issue tracker" => "dialog-warning-symbolic",
                "Source code" | "Manifest" => "text-x-script-symbolic",
                _ => "web-browser-symbolic",
            };
            let row = detail_row(tr(name), &url);
            row.set_subtitle_lines(1);
            row.set_activatable(true);
            row.set_tooltip_text(Some(&url));
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
            copy.add_css_class("flat");
            copy.set_valign(gtk::Align::Center);
            copy.set_tooltip_text(Some(tr("Copy link")));
            copy.update_property(&[gtk::accessible::Property::Label(tr("Copy link"))]);
            let address = url.clone();
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&address);
                button.announce(tr("Link copied"), gtk::AccessibleAnnouncementPriority::Low);
            });
            row.add_suffix(&copy);
            let open = gtk::Image::from_icon_name("go-jump-symbolic");
            row.add_suffix(&open);
            let parent = parent.clone();
            row.connect_activated(move |_| {
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
        section.append(&rows);
    }
    section.set_visible(section.first_child().is_some());
}

#[cfg(test)]
#[test]
fn project_details_use_only_the_height_of_the_license_and_links() {
    crate::with_gtk_test(|| {
        adw::init().unwrap();
        let project = gtk::Box::new(gtk::Orientation::Vertical, 12);
        show_project_details(
            &project,
            "Apache-2.0".into(),
            Some(true),
            Some(11_644),
            vec![
                ("Flathub page", "https://flathub.org/apps/example".into()),
                ("Project website", "https://example.org/".into()),
                ("Issue tracker", "https://example.org/issues".into()),
                ("Help", "https://example.org/help".into()),
                ("Source code", "https://example.org/source".into()),
            ],
            &glib::WeakRef::new(),
        );
        let license = project.first_child().and_downcast::<gtk::Box>().unwrap();
        let links = license
            .next_sibling()
            .and_downcast::<gtk::ListBox>()
            .unwrap();
        assert!(project.is_visible());
        assert!(links.next_sibling().is_none());
        for width in [320, 860] {
            let (_, license_height, _, _) = license.measure(gtk::Orientation::Vertical, width);
            let (_, links_height, _, _) = links.measure(gtk::Orientation::Vertical, width);
            let (_, total_height, _, _) = project.measure(gtk::Orientation::Vertical, width);
            assert!(links_height > license_height);
            assert_eq!(
                total_height,
                license_height + links_height + project.spacing()
            );
        }
    });
}

fn show_reviews(section: &gtk::Box, id: &str, version: String) {
    section.append(&home_label(tr("Reviews"), "title-3"));
    let status = home_label(tr("Loading reviews…"), "dim-label");
    section.append(&status);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    section.append(&content);
    let (status, content) = (status.downgrade(), content.downgrade());
    let id = id.to_owned();
    let locale = glib::language_names()
        .first()
        .map(ToString::to_string)
        .unwrap_or_else(|| "en_US".into());
    background(
        move || {
            worker()
                .ok()
                .and_then(|runner| store::reviews(&runner, &id, &locale, &version))
        },
        move |summary| {
            let (Some(status), Some(content)) = (status.upgrade(), content.upgrade()) else {
                return;
            };
            let Some(summary) = summary else {
                status.set_text(tr("Reviews are unavailable right now."));
                return;
            };
            if summary.ratings.iter().all(|&count| count == 0) && summary.reviews.is_empty() {
                status.set_text(tr("No reviews available."));
                return;
            }
            status.set_visible(false);
            populate_reviews(&content, summary);
        },
    );
}

fn populate_reviews(content: &gtk::Box, summary: store::ReviewSummary) {
    let total = summary.ratings.into_iter().fold(0u64, u64::saturating_add);
    if total > 0 {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 7);
        card.add_css_class("packlift-store-info-card");
        let star_width = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let max = summary.ratings.into_iter().max().unwrap_or(1);
        for (index, count) in summary.ratings.into_iter().enumerate().rev() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let bar = gtk::ProgressBar::new();
            bar.set_hexpand(true);
            bar.set_valign(gtk::Align::Center);
            bar.set_fraction(count as f64 / max as f64);
            let label = tr("{stars} stars · {count} ratings")
                .replace("{stars}", &(index + 1).to_string())
                .replace("{count}", &count.to_string());
            bar.set_tooltip_text(Some(&label));
            bar.update_property(&[gtk::accessible::Property::Label(&label)]);
            row.append(&bar);
            let stars = gtk::Label::new(Some(&"★".repeat(index + 1)));
            stars.set_xalign(0.0);
            stars.add_css_class("packlift-review-stars");
            star_width.add_widget(&stars);
            row.append(&stars);
            card.append(&row);
        }
        let label = if total == 1 {
            tr("1 rating").to_owned()
        } else {
            tr("{count} ratings").replace("{count}", &total.to_string())
        };
        card.append(&home_label(&label, "dim-label"));
        content.append(&card);
    }
    let mut reviews = summary.reviews.into_iter();
    let Some(first) = reviews.next() else {
        content.append(&home_label(
            tr("No written reviews available."),
            "dim-label",
        ));
        return;
    };
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .child(&review_card(first))
        .build();
    list.append(&row);
    if reviews.len() != 0 {
        let expander = adw::ExpanderRow::builder()
            .title(tr("More reviews"))
            .build();
        for review in reviews {
            expander.add_row(&review_card(review));
        }
        list.append(&expander);
    }
    content.append(&list);
}

fn review_card(review: store::AppReview) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 7);
    card.add_css_class("packlift-store-release");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let stars = home_label(&"★".repeat(review.stars), "packlift-review-stars");
    stars.set_single_line_mode(true);
    stars.update_property(&[gtk::accessible::Property::Label(
        &tr("{count} out of 5 stars").replace("{count}", &review.stars.to_string()),
    )]);
    header.append(&stars);
    if let Some(date) = review
        .timestamp
        .and_then(|timestamp| glib::DateTime::from_unix_utc(timestamp).ok())
        .and_then(|date| date.format("%x").ok())
    {
        let date = home_label(&date, "dim-label");
        date.set_hexpand(true);
        date.set_xalign(1.0);
        header.append(&date);
    }
    card.append(&header);
    if !review.title.is_empty() {
        card.append(&home_label(&review.title, "heading"));
    }
    if !review.author.is_empty() {
        card.append(&home_label(&review.author, "dim-label"));
    }
    if !review.body.is_empty() {
        let body = home_label(&review.body, "");
        body.set_selectable(true);
        card.append(&body);
    }
    card
}

#[cfg(test)]
#[test]
fn review_comments_follow_the_ratings_without_reserved_space() {
    crate::with_gtk_test(|| {
        adw::init().unwrap();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let comment = |body: &str| store::AppReview {
            stars: 5,
            title: "Works well".into(),
            body: body.into(),
            author: "Reviewer".into(),
            timestamp: None,
        };
        populate_reviews(
            &content,
            store::ReviewSummary {
                ratings: [5, 0, 0, 4, 16],
                reviews: vec![
                    comment(&"This is a long comment about the application. ".repeat(20)),
                    comment("This comment can be opened with More reviews."),
                ],
            },
        );
        let ratings = content.first_child().and_downcast::<gtk::Box>().unwrap();
        let list = ratings
            .next_sibling()
            .and_downcast::<gtk::ListBox>()
            .unwrap();
        let more = list
            .row_at_index(1)
            .and_downcast::<adw::ExpanderRow>()
            .unwrap();
        assert!(!more.is_expanded());
        for width in [320, 860] {
            let (_, ratings_height, _, _) = ratings.measure(gtk::Orientation::Vertical, width);
            let (_, list_height, _, _) = list.measure(gtk::Orientation::Vertical, width);
            let (_, total_height, _, _) = content.measure(gtk::Orientation::Vertical, width);
            assert_eq!(
                total_height,
                ratings_height + list_height + content.spacing()
            );
        }

        let ratings_only = gtk::Box::new(gtk::Orientation::Vertical, 12);
        populate_reviews(
            &ratings_only,
            store::ReviewSummary {
                ratings: [5, 0, 0, 4, 16],
                reviews: Vec::new(),
            },
        );
        let message = ratings_only
            .last_child()
            .and_downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(message.text(), tr("No written reviews available."));
        assert_eq!(message.prev_sibling(), ratings_only.first_child());
    });
}

fn merge_catalog_remotes(user: Vec<Remote>, system: Vec<Remote>) -> Vec<Remote> {
    let mut remotes = user;
    for remote in system {
        if !remotes.iter().any(|existing| {
            existing.destination == remote.destination
                && existing.name == remote.name
                && existing.url == remote.url
        }) {
            remotes.push(remote);
        }
    }
    remotes.sort_by_cached_key(|remote| (!remote.flathub, remote.title.to_lowercase()));
    remotes
}

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

fn store_fact(icon: &str, label: &str, value: &str, description: &str) -> gtk::FlowBoxChild {
    let child = gtk::FlowBoxChild::new();
    child.set_focusable(false);
    child.set_child(Some(&fact_content(icon, label, value, description)));
    child
}

fn fact_content(icon: &str, label: &str, value: &str, description: &str) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.add_css_class("packlift-store-fact-content");
    content.set_size_request(164, -1);
    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    badge.set_halign(gtk::Align::Center);
    badge.add_css_class("packlift-store-fact-badge");
    badge.append(&gtk::Image::from_icon_name(icon));
    badge.append(&home_label(value, "heading"));
    content.append(&badge);
    let heading = home_label(label, "heading");
    heading.set_halign(gtk::Align::Center);
    heading.set_justify(gtk::Justification::Center);
    content.append(&heading);
    let description = home_label(description, "caption");
    description.set_halign(gtk::Align::Center);
    description.set_justify(gtk::Justification::Center);
    description.set_max_width_chars(23);
    content.append(&description);
    content
}

fn permissions_fact(groups: Vec<(&'static str, String)>) -> gtk::FlowBoxChild {
    let broad_files = groups.iter().any(|(label, access)| {
        *label == "Files"
            && access.split(',').any(|path| {
                matches!(
                    path.trim().split(':').next(),
                    Some("host" | "home" | "host-os" | "host-etc")
                )
            })
    });
    let description = if broad_files {
        tr("Requests access to your home folder or other system files.")
    } else {
        tr("See what the app can access.")
    };
    let button = gtk::MenuButton::new();
    button.add_css_class("packlift-store-fact-button");
    button.set_child(Some(&fact_content(
        "security-high-symbolic",
        tr("Permissions"),
        if broad_files {
            tr("File access")
        } else {
            tr("Details")
        },
        description,
    )));
    let popover = gtk::Popover::new();
    button.set_popover(Some(&popover));
    let child = gtk::FlowBoxChild::new();
    child.set_focusable(false);
    child.set_child(Some(&button));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(4);
    content.set_margin_end(4);
    content.append(&home_label(tr("Permissions"), "heading"));
    if groups.is_empty() {
        let info = home_label(tr("No permissions listed by the repository"), "dim-label");
        info.set_max_width_chars(32);
        content.append(&info);
    } else {
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for (label, access) in groups {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
            row.append(&home_label(tr(label), "heading"));
            let details = home_label(&access, "dim-label");
            details.set_max_width_chars(36);
            details.set_selectable(true);
            row.append(&details);
            rows.append(&row);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .min_content_width(260)
            .max_content_width(340)
            .max_content_height(320)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .child(&rows)
            .build();
        content.append(&scroll);
    }
    popover.set_child(Some(&content));
    child
}

fn app_card(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: Remote,
    app: CatalogApp,
    color: usize,
) -> gtk::Button {
    catalog_app_card(window, remote, app, color, false)
}

fn catalog_app_card(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: Remote,
    app: CatalogApp,
    color: usize,
    featured: bool,
) -> gtk::Button {
    let button = gtk::Button::builder().hexpand(true).build();
    button.add_css_class("packlift-store-card");
    button.set_widget_name(&app.id);
    button.set_size_request(190, if featured { 110 } else { 76 });
    if featured {
        button.add_css_class("packlift-showcase-card");
    }
    button.update_property(&[gtk::accessible::Property::Label(&app.name)]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, if featured { 16 } else { 12 });
    content.set_valign(gtk::Align::Center);
    let (icon_size, frame_size) = if featured { (56, 70) } else { (40, 52) };
    content.append(&catalog_icon_frame(&app, icon_size, frame_size));
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
    summary.set_max_width_chars(if featured { 23 } else { 25 });
    text.append(&name);
    text.append(&summary);
    content.append(&text);
    button.set_child(Some(&content));
    let window = window.clone();
    button.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            show_app(&window, &remote, &app, color);
        }
    });
    button
}

const HUE_COUNT: usize = 8;
const APP_COLORS: [&str; HUE_COUNT * 4] = [
    "#175de0", "#00877d", "#139747", "#d08805", "#d6580d", "#cc193e", "#c41b85", "#7337d1",
    "#087fb8", "#057b58", "#6b9608", "#b89208", "#da4322", "#b91557", "#b321a2", "#5540d7",
    "#254bbf", "#0495a0", "#07904f", "#c27008", "#e04c36", "#b91436", "#cb288e", "#9026c5",
    "#2d6ac8", "#138e77", "#4a9f16", "#c18c16", "#c74917", "#d32b60", "#a829bd", "#674fce",
];

pub(super) fn app_color_styles() -> String {
    APP_COLORS
        .iter()
        .enumerate()
        .map(|(index, color)| {
            format!(".packlift-store-tint.color-{index} {{ background-color: mix(@card_bg_color, {color}, 0.14); }}\n")
        })
        .collect()
}

fn app_color(app_id: &str) -> usize {
    CARD_COLORS.with(|random| random.hash_one(app_id) as usize % APP_COLORS.len())
}

fn tint_app_card(widget: &impl IsA<gtk::Widget>, color: usize) {
    widget.add_css_class("packlift-store-tint");
    widget.add_css_class(&format!("color-{color}"));
}

fn catalog_icon_frame(app: &CatalogApp, icon_size: i32, frame_size: i32) -> gtk::Box {
    let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    frame.add_css_class("packlift-store-card-icon");
    frame.set_size_request(frame_size, frame_size);
    frame.set_hexpand(false);
    frame.set_valign(gtk::Align::Center);
    let icon = catalog_icon(app, icon_size);
    icon.set_hexpand(true);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    frame.append(&icon);
    frame
}

struct FeaturedApp {
    remote: Remote,
    app: CatalogApp,
    image: gtk::gdk::Texture,
    caption: String,
}

fn featured_app(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    featured: &FeaturedApp,
    color: usize,
) -> gtk::Button {
    let FeaturedApp {
        remote,
        app,
        image,
        caption,
    } = featured;
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    let intro = gtk::Box::new(gtk::Orientation::Vertical, 8);
    intro.set_hexpand(true);
    intro.set_halign(gtk::Align::Center);
    intro.set_valign(gtk::Align::Center);
    let icon = catalog_icon_frame(app, 48, 60);
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
    picture.set_paintable(Some(image));
    if !caption.is_empty() {
        picture.set_alternative_text(Some(caption));
    }
    hero.append(&picture);

    let remote = remote.clone();
    let app = app.clone();
    let parent = window.clone();
    let banner = gtk::Button::new();
    banner.add_css_class("packlift-store-feature");
    tint_app_card(&banner, color);
    banner.set_hexpand(true);
    banner.set_cursor_from_name(Some("pointer"));
    banner.set_child(Some(&hero));
    banner.update_property(&[
        gtk::accessible::Property::Label(&app.name),
        gtk::accessible::Property::Description(&app.summary),
    ]);
    banner.connect_clicked(move |_| {
        if let Some(window) = parent.upgrade() {
            show_app(&window, &remote, &app, color);
        }
    });
    banner
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
            queue_icon(&image, size, &key);
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
                    gtk::gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()
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
    repository: &gtk::DropDown,
    install: &gtk::MenuButton,
    preferred: &Remote,
    choices: Vec<StoreChoice>,
    install_app: impl Fn(Remote, CatalogApp) + Clone + 'static,
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
    let button = install.downgrade();
    let update = move |repository: &gtk::DropDown| {
        if let (Some(button), Some(source)) = (
            button.upgrade(),
            sources.get(repository.selected() as usize),
        ) {
            button.set_popover(Some(&install_scope_popover(source, install_app.clone())));
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
    source: &InstallSource,
    install_app: impl Fn(Remote, CatalogApp) + Clone + 'static,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_offset(-56, 0);
    let options = gtk::Box::new(gtk::Orientation::Vertical, 8);
    options.set_margin_top(4);
    options.set_margin_bottom(4);
    options.set_margin_start(4);
    options.set_margin_end(4);
    let heading = home_label(tr("Install scope"), "heading");
    heading.set_margin_start(4);
    options.append(&heading);
    let destinations = [
        (
            tr("For my account"),
            "avatar-default-symbolic",
            tr("Available only to your Linux account."),
        ),
        (
            tr("For the system"),
            "computer-symbolic",
            tr("Available to everyone on this computer. May require authorization."),
        ),
    ];
    for ((label, icon, description), choice) in destinations.into_iter().zip(&source.scopes) {
        let available = choice.is_some();
        let description = if available {
            description
        } else {
            tr("Unavailable from this repository.")
        };
        let button = scope_option(label, icon, description);
        button.set_sensitive(available);
        if let Some((remote, candidate)) = choice.clone() {
            let install_app = install_app.clone();
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                    install_app(remote.clone(), candidate.clone());
                }
            });
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
        note.set_max_width_chars(32);
        note.set_margin_start(4);
        options.append(&note);
    }
    popover.set_child(Some(&options));
    popover
}

fn install_store_app(
    window: &adw::ApplicationWindow,
    progress: &gtk::Box,
    source: Remote,
    candidate: CatalogApp,
) {
    let Some(application) = window.application().and_downcast::<adw::Application>() else {
        return;
    };
    run_job_with_progress(
        &application,
        window,
        &format!("{} · {}", tr("Installing program"), candidate.name),
        Some(progress),
        move |log| {
            let runner = Runner::new(None, log)?;
            store::install(&runner, &source, &candidate)?;
            let scope = match source.destination {
                Destination::User => "user",
                Destination::System => "system",
            };
            runner.write("Refreshing Flatpak state");
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
}

fn scope_option(label: &str, icon: &str, description: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("packlift-scope-option");
    button.update_property(&[
        gtk::accessible::Property::Label(label),
        gtk::accessible::Property::Description(description),
    ]);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let icon = gtk::Image::from_icon_name(icon);
    icon.add_css_class("packlift-scope-icon");
    icon.set_valign(gtk::Align::Center);
    row.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(&home_label(label, "heading"));
    let subtitle = home_label(description, "dim-label");
    subtitle.set_max_width_chars(29);
    text.append(&subtitle);
    row.append(&text);
    button.set_child(Some(&row));
    button
}

fn uninstall_scope_popover(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    progress: &glib::WeakRef<gtk::Box>,
    app_name: &str,
    records: Vec<Value>,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_offset(-56, 0);
    let options = gtk::Box::new(gtk::Orientation::Vertical, 8);
    options.set_margin_top(4);
    options.set_margin_bottom(4);
    options.set_margin_start(4);
    options.set_margin_end(4);
    let title = tr("Uninstall {app}?").replace("{app}", app_name);
    let heading = home_label(&title, "heading");
    heading.set_max_width_chars(32);
    options.append(&heading);

    let labels = records
        .iter()
        .map(|record| {
            let scope = crate::flatpak_scope_label(record);
            match record["target"]
                .as_str()
                .and_then(|reference| reference.split_once('/'))
            {
                Some((_, details)) => format!("{scope} · {details}"),
                None => scope,
            }
        })
        .collect::<Vec<_>>();
    let scope = if labels.len() == 1 {
        let label = home_label(&labels[0], "dim-label");
        label.set_max_width_chars(32);
        options.append(&label);
        None
    } else {
        let strings = labels.iter().map(String::as_str).collect::<Vec<_>>();
        let scope = gtk::DropDown::from_strings(&strings);
        options.append(&labeled_control(tr("Installation to remove"), &scope));
        Some(scope)
    };

    let records = Rc::new(records);
    for (delete_data, icon, label, description) in [
        (false, "folder-symbolic", tr("Keep app data"), tr("Keep saved data and settings for a future installation.")),
        (true, "user-trash-symbolic", tr("Delete app data"), tr("This permanently deletes saved data and settings for this app in your account, including data shared with other installations of the same app.")),
    ] {
        let button = scope_option(label, icon, description);
        if delete_data {
            button.add_css_class("destructive-action");
        }
        let (window, progress, popover, records) = (
            window.clone(), progress.clone(), popover.downgrade(), records.clone(),
        );
        let scope = scope.as_ref().map(ObjectExt::downgrade);
        button.connect_clicked(move |_| {
            let (Some(window), Some(progress), Some(popover)) =
                (window.upgrade(), progress.upgrade(), popover.upgrade())
            else { return; };
            let index = if let Some(scope) = &scope {
                let Some(scope) = scope.upgrade() else { return; };
                scope.selected() as usize
            } else { 0 };
            let Some(record) = records.get(index).cloned() else { return; };
            let Some(application) = window.application().and_downcast::<adw::Application>() else { return; };
            popover.popdown();
            run_job_with_progress(
                &application,
                &window,
                &format!("{} · {}", tr("Removing from your library"), record["name"].as_str().unwrap_or(tr("Program"))),
                Some(&progress),
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
        options.append(&button);
    }
    popover.set_child(Some(&options));
    popover
}

fn bind_gallery(gallery: &gtk::Box, shots: Vec<Screenshot>) {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let heading = home_label(tr("Screenshots"), "title-3");
    heading.set_hexpand(true);
    header.append(&heading);
    let previous = gtk::Button::from_icon_name("go-previous-symbolic");
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    for (button, name) in [(&previous, tr("Previous")), (&next, tr("Next"))] {
        button.set_tooltip_text(Some(name));
        button.update_property(&[gtk::accessible::Property::Label(name)]);
        header.append(button);
    }
    gallery.append(&header);

    let single = shots.len() == 1;
    let previews = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    for shot in &shots {
        let frame = screenshot_frame(shot, single);
        frame.set_hexpand(single);
        previews.append(&frame);
    }
    let viewport = gtk::Viewport::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    if single {
        let layout = adw::Clamp::builder()
            .maximum_size(820)
            .child(&previews)
            .build();
        viewport.set_child(Some(&layout));
    } else {
        viewport.set_child(Some(&previews));
    }
    let strip = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(if single {
            gtk::PolicyType::Never
        } else {
            gtk::PolicyType::Automatic
        })
        .overlay_scrolling(false)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&viewport)
        .build();
    gallery.append(&strip);

    let adjustment = strip.hadjustment();
    for (button, direction) in [(&previous, -1.0), (&next, 1.0)] {
        let scroll = adjustment.downgrade();
        button.connect_clicked(move |_| {
            if let Some(scroll) = scroll.upgrade() {
                scroll.set_value(scroll.value() + direction * scroll.page_size() * 0.8);
            }
        });
    }
    let (previous, next) = (previous.downgrade(), next.downgrade());
    let update = move |scroll: &gtk::Adjustment| {
        let (Some(previous), Some(next)) = (previous.upgrade(), next.upgrade()) else {
            return;
        };
        let lower = scroll.lower();
        let upper = scroll.upper() - scroll.page_size();
        let scrollable = upper - lower > 1.0;
        let back = scroll.value() - lower > 1.0;
        let forward = upper - scroll.value() > 1.0;
        if previous.has_focus() && !back && forward {
            next.set_sensitive(true);
            next.grab_focus();
        } else if next.has_focus() && !forward && back {
            previous.set_sensitive(true);
            previous.grab_focus();
        }
        previous.set_visible(scrollable);
        next.set_visible(scrollable);
        previous.set_sensitive(back);
        next.set_sensitive(forward);
    };
    update(&adjustment);
    adjustment.connect_changed(update.clone());
    adjustment.connect_value_changed(update);
}

fn screenshot_frame(shot: &Screenshot, single: bool) -> gtk::Box {
    const THUMBNAIL_HEIGHT: i32 = 260;
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    let name = if shot.caption.is_empty() {
        tr("Screenshots")
    } else {
        &shot.caption
    };
    picture.set_alternative_text(Some(name));
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("packlift-store-shot");
    overlay.set_overflow(gtk::Overflow::Hidden);
    let canvas = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if single {
        canvas.set_height_request(392);
    } else {
        let width = (shot.aspect_ratio.unwrap_or(1.6) * f64::from(THUMBNAIL_HEIGHT))
            .round()
            .clamp(118.0, 540.0) as i32;
        canvas.set_size_request(width, THUMBNAIL_HEIGHT);
    }
    overlay.set_child(Some(&canvas));
    overlay.add_overlay(&picture);
    frame.append(&overlay);
    let caption = gtk::Label::new(Some(tr("Loading screenshots…")));
    caption.add_css_class("dim-label");
    if single {
        caption.set_wrap(true);
        caption.set_lines(2);
        caption.set_size_request(-1, 40);
    } else {
        caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
        caption.set_lines(1);
        caption.set_size_request(-1, 20);
        caption.set_max_width_chars(22);
        caption.set_tooltip_text((!shot.caption.is_empty()).then_some(shot.caption.as_str()));
    }
    frame.append(&caption);
    let (url, fallback) = if single && shot.large_url != shot.url {
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
