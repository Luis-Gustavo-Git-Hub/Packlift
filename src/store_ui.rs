use std::{
    cell::{Cell, RefCell},
    collections::{hash_map::RandomState, BTreeSet, HashMap, VecDeque},
    hash::BuildHasher,
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
    back_to_previous, home_label, i18n::tr, page_navigation, page_shortcut, render_home,
    run_job_with_progress, show_host_access_dialog, text_button,
};

struct StoreView {
    window: glib::WeakRef<adw::ApplicationWindow>,
    search: gtk::SearchEntry,
    categories: gtk::Box,
    discover_heading: gtk::Label,
    feature: gtk::Box,
    carousel: adw::Carousel,
    list: gtk::FlowBox,
    status: gtk::Label,
    scroll: gtk::ScrolledWindow,
    previous: gtk::Button,
    next: gtk::Button,
    page_label: gtk::Label,
    remotes: RefCell<Vec<Remote>>,
    installed: RefCell<Option<BTreeSet<String>>>,
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
    body.append(&search_row);

    let status = home_label("", "dim-label");
    status.set_selectable(true);
    body.append(&status);
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
    let discover_heading = home_label(tr("Trending"), "title-2");
    discover_heading.set_visible(false);
    catalog.append(&discover_heading);
    catalog.append(&list);
    let categories = gtk::Box::new(gtk::Orientation::Vertical, 22);
    categories.set_visible(false);
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
        categories,
        discover_heading,
        feature,
        carousel,
        list,
        status,
        scroll,
        previous,
        next,
        page_label,
        remotes: RefCell::new(Vec::new()),
        installed: RefCell::new(None),
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
    view.load_remotes();
    StoreSession {
        window: window.downgrade(),
        toolbar,
        search: view.search.clone(),
    }
}

impl StoreView {
    fn set_status(&self, message: &str) {
        self.status.set_text(message);
        self.status.set_visible(!message.is_empty());
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
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
            move || worker().and_then(|runner| store::catalog_remotes(&runner, Destination::User)),
            move |result| match result {
                Ok(user_remotes) => {
                    *view.remotes.borrow_mut() = user_remotes;
                    view.load_categories();
                    view.new_search();
                    view.load_installed();
                    view.load_system_remotes();
                }
                Err(error) => {
                    view.set_status(&error.to_string());
                    view.set_busy(false);
                }
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
                let changed = view.installed.borrow().as_ref() != Some(&installed);
                *view.installed.borrow_mut() = Some(installed.clone());
                if let Some(search) = view.catalog_search.borrow_mut().as_mut() {
                    search.exclude_installed(&installed);
                }
                if changed
                    && !view.busy.get()
                    && view
                        .catalog_search
                        .borrow()
                        .as_ref()
                        .is_some_and(CatalogSearch::is_explore)
                {
                    view.load_categories();
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
                let remotes =
                    merge_catalog_remotes(view.remotes.borrow().clone(), system_remotes);
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
        if !keep_position {
            self.clear_results();
            self.page_label.set_text("");
            self.set_status(tr("Searching the store…"));
        }
        let explore = catalog_search.is_explore() && page == 1;
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
                    Ok(results) => {
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
                        view.categories.set_visible(explore);
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
        self.categories.set_visible(false);
        self.discover_heading.set_visible(false);
    }

    fn load_categories(&self) {
        while let Some(section) = self.categories.first_child() {
            self.categories.remove(&section);
        }
        let Some(remote) = self
            .remotes
            .borrow()
            .iter()
            .find(|remote| remote.flathub)
            .cloned()
        else {
            return;
        };
        let Some(installed) = self.installed.borrow().clone() else {
            return;
        };
        let window = &self.window;
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
        ]
        .into_iter()
        .enumerate()
        {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
            section.set_visible(false);
            self.categories.append(&section);
            if key == "daily" {
                load_daily_feature(&section, window, &remote, &installed);
                continue;
            }
            section.append(&home_label(tr(label), "title-2"));
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
                        &section,
                        &panel,
                        window,
                        &remote,
                        &installed,
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
                    &section,
                    &panel,
                    window,
                    &remote,
                    &installed,
                    (key, label),
                    index,
                );
            }
        }
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

fn load_discovery_grid(
    section: &gtk::Box,
    panel: &gtk::Box,
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: &Remote,
    installed: &BTreeSet<String>,
    (collection, title): (&'static str, &'static str),
    color_offset: usize,
) {
    let (section, panel) = (section.downgrade(), panel.downgrade());
    let (window, remote, installed) = (window.clone(), remote.clone(), installed.clone());
    background(
        move || {
            worker()
                .map(|runner| store::discovery_apps(&runner, collection, &installed))
                .unwrap_or_default()
        },
        move |apps| {
            let (Some(section), Some(panel)) = (section.upgrade(), panel.upgrade()) else {
                return;
            };
            if apps.is_empty() {
                panel.set_visible(false);
                return;
            }
            let tiles = discovery_grid();
            let featured = collection == "mobile"
                || collection == "game-launchers"
                || collection.starts_with("category/Game");
            if featured {
                tiles.set_row_spacing(14);
                tiles.set_column_spacing(14);
            }
            for (position, app) in apps.into_iter().enumerate() {
                let color = (color_offset * 7 + position * 5) % APP_COLORS.len();
                tiles.append(&catalog_app_card(
                    &window,
                    remote.clone(),
                    app,
                    color,
                    featured,
                ));
            }
            panel.append(&tiles);
            let more = text_button(tr("More"));
            more.add_css_class("pill");
            more.set_halign(gtk::Align::Center);
            let accessible = tr("More in {category}").replace("{category}", tr(title));
            more.update_property(&[gtk::accessible::Property::Label(&accessible)]);
            more.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    show_collection(&window, &remote, collection, tr(title));
                }
            });
            panel.append(&more);
            section.set_visible(true);
        },
    );
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
    let load = Rc::new({
        let (tiles, status, scroll) = (
            tiles.downgrade(),
            status.downgrade(),
            scroll.downgrade(),
        );
        let window = window.downgrade();
        let remote = remote.clone();
        let page = page.clone();
        let has_next = has_next.clone();
        let busy = busy.clone();
        move |number: usize| {
            if busy.get() {
                return;
            }
            if number > 1 && !has_next.get() {
                return;
            }
            let (
                Some(tiles),
                Some(status),
                Some(_scroll),
            ) = (
                tiles.upgrade(),
                status.upgrade(),
                scroll.upgrade(),
            )
            else {
                return;
            };
            busy.set(true);
            if number == 1 {
                status.set_visible(true);
                status.set_text(tr("Searching the store…"));
            }
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
                            if number == 1 {
                                status.set_text(&error.to_string());
                            }
                        }
                    }
                },
            );
        }
    });

    let vadjustment = scroll.vadjustment();
    let load_more = load.clone();
    let page_for_scroll = page.clone();
    vadjustment.connect_value_changed(move |adj| {
        if adj.upper() > adj.page_size() && adj.value() + adj.page_size() >= adj.upper() - 240.0 {
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
    installed: &BTreeSet<String>,
) {
    let (section, window) = (section.downgrade(), window.clone());
    let (remote, installed) = (remote.clone(), installed.clone());
    background(
        move || {
            let runner = worker().ok()?;
            let mut apps = store::discovery_apps(&runner, "trending", &installed);
            if apps.is_empty() {
                return None;
            }
            let days = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs()
                / 86400;
            let offset = days as usize % apps.len();
            apps.rotate_left(offset);
            apps.into_iter().find_map(|app| {
                let details = store::app_details(&runner, &remote, &app).ok()?;
                let shot = details.screenshots.first()?;
                let bytes = store::screenshot_bytes(&runner, &shot.url).ok()?;
                Some((remote.clone(), app, bytes, shot.caption.clone()))
            })
        },
        move |result| {
            let (Some(section), Some((remote, app, bytes, caption))) = (section.upgrade(), result)
            else {
                return;
            };
            let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
            let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                &stream,
                1440,
                1440,
                true,
                gio::Cancellable::NONE,
            ) else {
                return;
            };
            let featured = FeaturedApp {
                remote,
                app,
                image: gtk::gdk::Texture::for_pixbuf(&pixbuf),
                caption,
            };
            section.append(&home_label(tr("App of the Day"), "title-2"));
            section.append(&featured_app(
                &window,
                &featured,
                app_color(&featured.app.id),
            ));
            section.set_visible(true);
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
    let loading_name = format!("packlift-store-loading-{}", app.id);
    window.set_widget_name(&loading_name);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Store"), "")));
    back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);
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

    let window = window.downgrade();
    let remote = remote.clone();
    let app = app.clone();
    let (detail_remote, detail_app) = (remote.clone(), app.clone());
    background(
        move || worker().and_then(|runner| store::app_details(&runner, &detail_remote, &detail_app)),
        move |details| {
            let Some(window) = window.upgrade() else {
                return;
            };
            if window.widget_name() == loading_name.as_str() {
                show_app_details(&window, &remote, &app, color, details);
            }
        },
    );
}

fn show_app_details(
    window: &adw::ApplicationWindow,
    remote: &Remote,
    app: &CatalogApp,
    color: usize,
    details_result: std::result::Result<store::AppDetails, packlift::core::PackliftError>,
) {
    window.set_widget_name("packlift-store-details");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 24);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(18);
    body.set_margin_bottom(24);

    let hero = gtk::Box::new(gtk::Orientation::Vertical, 16);
    hero.add_css_class("packlift-store-detail-hero");
    tint_app_card(&hero, color);
    let identity = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    identity.append(&catalog_icon_frame(app, 68, 80));
    let title = gtk::Box::new(gtk::Orientation::Vertical, 4);
    title.set_hexpand(true);
    title.set_valign(gtk::Align::Center);
    title.append(&home_label(&app.name, "title-1"));
    if !app.summary.is_empty() && app.summary != app.id {
        title.append(&home_label(&app.summary, "dim-label"));
    }
    let developer = home_label("", "dim-label");
    // Reserve the author line before metadata arrives so the hero stays put.
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
        .max_children_per_line(6)
        .homogeneous(true)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    facts.set_vexpand(false);
    // Keep room for two rows of facts while repository data is loading.
    facts.set_size_request(-1, 112);
    facts.set_visible(true);
    hero.append(&facts);
    body.append(&hero);

    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let gallery_status = home_label(tr("Loading screenshots…"), "dim-label");
    gallery_status.set_size_request(-1, 280);
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

    let releases = gtk::Box::new(gtk::Orientation::Vertical, 12);
    releases.set_visible(false);
    body.append(&releases);
    let (sources_section, sources) = detail_list(tr("Sources"));
    body.append(&sources_section);
    let links_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    links_section.set_visible(false);
    links_section.append(&home_label(tr("Links"), "title-3"));
    let links = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .homogeneous(true)
        .row_spacing(4)
        .column_spacing(6)
        .build();
    links_section.append(&links);
    body.append(&links_section);
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
        .maximum_size(1120)
        .tightening_threshold(940)
        .child(&body)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .hexpand(true)
        .child(&layout)
        .build();
    let (suggestions_layout, rails) = detail_suggestion_layout(&scroll, &similar_section);
    toolbar.set_content(Some(&suggestions_layout));
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
                    indicators.append(&permissions_fact(groups));
                    indicators.set_visible(true);
                }
            },
        );
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
                    store::suggested_apps(
                        &runner,
                        &catalog_source,
                        &id,
                        category.as_deref(),
                    )
                })
                .unwrap_or_default()
        },
        move |apps| {
            for (index, (remote, app)) in apps.into_iter().enumerate() {
                let palette = color / HUE_COUNT * HUE_COUNT;
                let suggestion_color = palette + (color + index + 1) % HUE_COUNT;
                let child = gtk::FlowBoxChild::new();
                child.set_focusable(false);
                child.set_child(Some(&suggested_app_card(
                    &parent_for_cards,
                    remote.clone(),
                    &app,
                    suggestion_color,
                )));
                similar.append(&child);
                let rail = &rails[index % 2];
                let tile = suggested_app_card(&parent_for_cards, remote, &app, suggestion_color);
                rail.append(&tile);
            }
            update_suggestion_visibility(&suggestions_layout, &similar_section, &rails);
        },
    );
    match details_result {
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
            if details.screenshots.is_empty() {
                gallery_status
                    .set_text(tr("No screenshots are available from this repository."));
            } else {
                gallery.remove(&gallery_status);
                bind_gallery(&gallery, details.screenshots);
            }
        }
        Err(_) => gallery_status.set_text(tr("App information could not be loaded.")),
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
    rows: &gtk::FlowBox,
    parent: &glib::WeakRef<adw::ApplicationWindow>,
    links: Vec<(&'static str, String)>,
) {
    section.set_visible(!links.is_empty());
    for (name, url) in links {
        let name = tr(name);
        let tile = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        tile.set_size_request(190, -1);
        tile.add_css_class("packlift-store-link");
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&gtk::Image::from_icon_name("go-jump-symbolic"));
        let title = home_label(name, "");
        title.set_wrap(false);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&title);
        let open = gtk::Button::new();
        open.add_css_class("flat");
        open.set_hexpand(true);
        open.set_child(Some(&content));
        open.set_tooltip_text(Some(&url));
        open.update_property(&[
            gtk::accessible::Property::Label(name),
            gtk::accessible::Property::Description(&url),
        ]);
        tile.append(&open);
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.add_css_class("flat");
        copy.set_tooltip_text(Some(tr("Copy link")));
        copy.update_property(&[
            gtk::accessible::Property::Label(tr("Copy link")),
            gtk::accessible::Property::Description(name),
        ]);
        tile.append(&copy);
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
        let row = gtk::FlowBoxChild::new();
        row.set_focusable(false);
        row.set_child(Some(&tile));
        rows.append(&row);
    }
}

type StoreChoice = (Remote, CatalogApp);

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

fn store_fact(
    icon: &str,
    label: &str,
    value: &str,
    description: &str,
    detail: Option<&str>,
) -> gtk::FlowBoxChild {
    let (child, popover) = fact_chip(icon, label, value);
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
    child
}

fn permissions_fact(groups: Vec<(&'static str, String)>) -> gtk::FlowBoxChild {
    let (child, popover) = fact_chip("security-high-symbolic", tr("Permissions"), tr("Details"));
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

fn fact_chip(icon: &str, label: &str, value: &str) -> (gtk::FlowBoxChild, gtk::Popover) {
    let button = gtk::MenuButton::new();
    button.add_css_class("packlift-store-fact");
    button.set_hexpand(true);
    let name = format!("{label} · {value}");
    button.set_tooltip_text(Some(&name));
    button.update_property(&[gtk::accessible::Property::Label(&name)]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name(icon);
    icon.add_css_class("dim-label");
    content.append(&icon);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    for (text, style, lines) in [(label, "dim-label", 2), (value, "heading", 1)] {
        let text = home_label(text, style);
        text.set_max_width_chars(14);
        text.set_lines(lines);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&text);
    }
    content.append(&labels);
    content.set_overflow(gtk::Overflow::Hidden);
    button.set_child(Some(&content));
    let popover = gtk::Popover::new();
    button.set_popover(Some(&popover));
    let child = gtk::FlowBoxChild::new();
    child.set_focusable(false);
    child.set_child(Some(&button));
    (child, popover)
}

fn detail_suggestion_layout(
    scroll: &gtk::ScrolledWindow,
    inline: &gtk::Box,
) -> (adw::BreakpointBin, [gtk::Box; 2]) {
    let rails = std::array::from_fn(|_| {
        let rail = gtk::Box::new(gtk::Orientation::Vertical, 12);
        rail.set_width_request(220);
        rail.set_hexpand(false);
        rail.set_valign(gtk::Align::Start);
        rail.set_margin_top(18);
        rail.set_margin_start(12);
        rail.set_margin_end(12);
        rail.set_margin_bottom(18);
        rail.set_visible(false);

        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let adjustment = scroll.vadjustment().downgrade();
        wheel.connect_scroll(move |wheel, _, dy| {
            let Some(adjustment) = adjustment.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let distance = if wheel.unit() == gtk::gdk::ScrollUnit::Wheel {
                dy * 56.0
            } else {
                dy
            };
            adjustment.set_value(adjustment.value() + distance);
            glib::Propagation::Stop
        });
        rail.add_controller(wheel);
        rail
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&rails[0]);
    row.append(scroll);
    row.append(&rails[1]);
    let layout = adw::Clamp::builder()
        .maximum_size(1620)
        .tightening_threshold(1480)
        .child(&row)
        .build();
    let responsive = adw::BreakpointBin::builder()
        .width_request(360)
        .height_request(300)
        .child(&layout)
        .build();
    let condition = adw::BreakpointCondition::new_and(
        adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MinWidth,
            1320.0,
            adw::LengthUnit::Px,
        ),
        adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MinHeight,
            420.0,
            adw::LengthUnit::Px,
        ),
    );
    responsive.add_breakpoint(adw::Breakpoint::new(condition));
    let (alternate, sidebars) = (inline.clone(), rails.clone());
    responsive.connect_current_breakpoint_notify(move |responsive| {
        update_suggestion_visibility(responsive, &alternate, &sidebars);
    });
    (responsive, rails)
}

fn update_suggestion_visibility(
    layout: &adw::BreakpointBin,
    inline: &gtk::Box,
    rails: &[gtk::Box; 2],
) {
    let available = rails[0].first_child().is_some();
    let wide = layout.current_breakpoint().is_some();
    inline.set_visible(available && !wide);
    for rail in rails {
        rail.set_visible(available && wide);
    }
}

fn suggested_app_card(
    window: &glib::WeakRef<adw::ApplicationWindow>,
    remote: Remote,
    app: &CatalogApp,
    color: usize,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("packlift-store-suggestion");
    button.add_css_class(&format!("color-{color}"));
    button.set_size_request(150, 92);
    button.set_cursor_from_name(Some("pointer"));
    button.set_tooltip_text(Some(&app.name));
    button.update_property(&[
        gtk::accessible::Property::Label(&app.name),
        gtk::accessible::Property::Description(&app.summary),
    ]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.set_valign(gtk::Align::Center);
    content.append(&catalog_icon_frame(app, 36, 48));

    let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    let title = home_label(&app.name, "heading");
    title.set_lines(1);
    title.set_max_width_chars(14);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    labels.append(&title);
    if !app.summary.is_empty() && app.summary != app.id {
        let summary = home_label(&app.summary, "dim-label");
        summary.add_css_class("caption");
        summary.set_lines(2);
        summary.set_max_width_chars(14);
        summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&summary);
    }
    content.append(&labels);
    button.set_child(Some(&content));
    let (window, app) = (window.clone(), app.clone());
    button.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            show_app(&window, &remote, &app, color);
        }
    });
    button
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
    tint_app_card(&button, color);
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
            format!(".packlift-store-tint.color-{index} {{ background-color: mix(@window_bg_color, {color}, 0.78); }}\nbutton.packlift-store-suggestion.color-{index} {{ border-left-color: {color}; }}\n")
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
        tr("Installing program"),
        Some(progress),
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
                tr("Removing from your library"),
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
