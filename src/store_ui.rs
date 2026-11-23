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
        .vscrollbar_policy(gtk::PolicyType::External)
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
                    view.load_categories();
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
        let view = self.clone();
        background(
            move || crate::installer().and_then(|installer| installer.installed_flatpak_ids()),
            move |result| {
                if let Ok(installed) = result {
                    let changed = view.installed.borrow().as_ref() != Some(&installed);
                    let refresh = changed
                        && !view.busy.get()
                        && view
                            .catalog_search
                            .borrow()
                            .as_ref()
                            .is_some_and(CatalogSearch::is_explore);
                    if refresh {
                        if let Some(search) = view.catalog_search.borrow_mut().as_mut() {
                            search.exclude_installed(&installed);
                        }
                    }
                    *view.installed.borrow_mut() = Some(installed);
                    if refresh {
                        view.load_categories();
                        view.load_catalog_page(view.page.get(), true);
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
        let categories = self.categories.downgrade();
        let window = self.window.clone();
        for (index, (key, label)) in [
            ("Office", "Productivity"),
            ("Graphics", "Graphics & Photography"),
            ("AudioVideo", "Audio & Video"),
            ("Game", "Games"),
            ("Education", "Education"),
            ("Network", "Networking"),
            ("Science", "Science"),
            ("System", "System"),
            ("Utility", "Utilities"),
        ]
        .into_iter()
        .enumerate()
        {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
            section.set_visible(false);
            self.categories.append(&section);
            let section = section.downgrade();
            let (categories, window, remote, installed) = (
                categories.clone(),
                window.clone(),
                remote.clone(),
                installed.clone(),
            );
            background(
                move || {
                    worker()
                        .map(|runner| store::category_apps(&runner, key, &installed))
                        .unwrap_or_default()
                },
                move |apps| {
                    let (Some(_), Some(section)) = (categories.upgrade(), section.upgrade()) else {
                        return;
                    };
                    if apps.is_empty() {
                        return;
                    }
                    section.append(&home_label(tr(label), "title-2"));
                    let tiles = gtk::FlowBox::builder()
                        .selection_mode(gtk::SelectionMode::None)
                        .min_children_per_line(1)
                        .max_children_per_line(3)
                        .homogeneous(true)
                        .row_spacing(10)
                        .column_spacing(10)
                        .build();
                    for (position, app) in apps.into_iter().enumerate() {
                        let color = (index * 7 + position * 5) % APP_COLORS.len();
                        tiles.append(&app_card(&window, remote.clone(), app, color));
                    }
                    section.append(&tiles);
                    section.set_visible(true);
                },
            );
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
    let progress = gtk::Box::new(gtk::Orientation::Vertical, 0);
    progress.set_visible(false);
    let progress_layout = adw::Clamp::builder()
        .maximum_size(560)
        .child(&progress)
        .build();
    progress
        .bind_property("visible", &progress_layout, "visible")
        .sync_create()
        .build();
    hero.append(&progress_layout);

    let facts = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(6)
        .homogeneous(true)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    facts.set_vexpand(false);
    facts.set_visible(false);
    hero.append(&facts);
    body.append(&hero);

    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let gallery_status = home_label(tr("Loading screenshots…"), "dim-label");
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
    window.set_widget_name("packlift-store-details");
    window.set_title(Some(&app.name));
    let installed = Rc::new(RefCell::new(Vec::new()));
    let installed_state = installed.clone();
    let controls = install_controls.downgrade();
    let refresh_button = uninstall.downgrade();
    let app_id = app.id.clone();
    let refresh_installation = move || {
        let id = app_id.clone();
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
                    *state.borrow_mut() = records;
                    controls.set_visible(!is_installed);
                    button.set_visible(is_installed);
                }
                controls.set_sensitive(true);
                button.set_sensitive(true);
                if button.is_visible() && button.root().is_some_and(|root| root.focus().is_none()) {
                    button.grab_focus();
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
    let (controls, action) = (install_controls.downgrade(), uninstall.downgrade());
    progress.connect_visible_notify(move |progress| {
        if progress.is_visible() {
            if let (Some(controls), Some(action)) = (controls.upgrade(), action.upgrade()) {
                controls.set_sensitive(false);
                action.set_sensitive(false);
            }
        } else {
            refresh_installation();
        }
    });
    let dialog = window.downgrade();
    let options = install_options.downgrade();
    let job = progress.downgrade();
    let name = app.name.clone();
    uninstall.connect_clicked(move |button| {
        if let (Some(window), Some(options), Some(progress)) =
            (dialog.upgrade(), options.upgrade(), job.upgrade())
        {
            choose_uninstall_destination(
                &window,
                &progress,
                &options,
                button,
                &name,
                installed.borrow().clone(),
            );
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
    let source = remote.clone();
    let catalog_source = remote.clone();
    let selected = app.clone();
    let id = app.id.clone();
    let parent = window.downgrade();
    background(
        move || worker().and_then(|runner| store::app_details(&runner, &source, &selected)),
        move |result| {
            let category = result
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
                        let tile =
                            suggested_app_card(&parent_for_cards, remote, &app, suggestion_color);
                        for property in ["width-request", "height-request"] {
                            rail.bind_property("width-request", &tile, property)
                                .sync_create()
                                .build();
                        }
                        rail.append(&tile);
                    }
                    update_suggestion_visibility(&suggestions_layout, &similar_section, &rails);
                },
            );
            match result {
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
                Err(_) => gallery_status.set_text(tr("Screenshots could not be loaded.")),
            }
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
        rail.set_size_request(154, -1);
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
    let responsive = adw::BreakpointBin::builder()
        .width_request(360)
        .height_request(300)
        .child(&row)
        .build();
    for (width, height, size) in [
        (1320.0, 540.0, 154),
        (1440.0, 666.0, 200),
        (1540.0, 792.0, 242),
        (1680.0, 918.0, 284),
        (1800.0, 972.0, 302),
    ] {
        let condition = adw::BreakpointCondition::new_and(
            adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MinWidth,
                width,
                adw::LengthUnit::Px,
            ),
            adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MinHeight,
                height,
                adw::LengthUnit::Px,
            ),
        );
        let breakpoint = adw::Breakpoint::new(condition);
        breakpoint.add_setters(&[
            (&rails[0], "width-request", size),
            (&rails[1], "width-request", size),
        ]);
        responsive.add_breakpoint(breakpoint);
    }
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
    tint_app_card(&button, color);
    button.set_size_request(154, 154);
    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
    button.set_cursor_from_name(Some("pointer"));
    button.set_tooltip_text(Some(&app.name));
    button.update_property(&[gtk::accessible::Property::Label(&app.name)]);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    let icon = catalog_icon_frame(app, 58, 76);
    icon.set_halign(gtk::Align::Center);
    content.append(&icon);
    let title = home_label(&app.name, "heading");
    title.set_justify(gtk::Justification::Center);
    title.set_xalign(0.5);
    title.set_lines(2);
    title.set_max_width_chars(13);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&title);
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
    let button = gtk::Button::builder().hexpand(true).build();
    button.add_css_class("packlift-store-card");
    tint_app_card(&button, color);
    button.set_widget_name(&app.id);
    button.set_size_request(190, 76);
    button.update_property(&[gtk::accessible::Property::Label(&app.name)]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_valign(gtk::Align::Center);
    content.append(&catalog_icon_frame(&app, 40, 52));
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
            show_app(&window, &remote, &app, color);
        }
    });
    button
}

const HUE_COUNT: usize = 8;
const APP_COLORS: [&str; HUE_COUNT * 4] = [
    "#195fc9", "#008978", "#268338", "#ae7900", "#be4d15", "#ba2e3e", "#b03383", "#7142c2",
    "#1885bc", "#147a55", "#647b16", "#b88e14", "#b96622", "#ab294f", "#9b3875", "#5953bf",
    "#2b50a3", "#198b87", "#318a58", "#a16518", "#c35435", "#a33037", "#aa4f90", "#8439a7",
    "#476bb4", "#187d68", "#518338", "#a88026", "#a55032", "#c23f54", "#923e8d", "#6f53ac",
];

pub(super) fn app_color_styles() -> String {
    APP_COLORS
        .iter()
        .enumerate()
        .map(|(index, color)| {
            format!(".packlift-store-tint.color-{index} {{ background-color: mix(@window_bg_color, {color}, 0.64); }}\n")
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
        let button = gtk::Button::new();
        button.add_css_class("packlift-scope-option");
        button.set_sensitive(available);
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

fn dismiss_store_options(
    container: &gtk::Box,
    controls: &adw::Clamp,
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
    progress: &gtk::Box,
    container: &gtk::Box,
    open_options: &gtk::Button,
    app_name: &str,
    records: Vec<Value>,
) {
    if records.is_empty() || window.has_css_class("packlift-running-job") {
        return;
    }

    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("packlift-store-uninstall");
    let layout = adw::Clamp::builder().maximum_size(560).child(&card).build();

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let trash = gtk::Image::from_icon_name("user-trash-symbolic");
    trash.set_pixel_size(20);
    trash.set_valign(gtk::Align::Center);
    trash.add_css_class("packlift-uninstall-icon");
    heading.append(&trash);
    let title = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let question = tr("Uninstall {app}?").replace("{app}", app_name);
    title.append(&home_label(&question, "title-3"));
    heading.append(&title);
    card.append(&heading);

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
        title.append(&home_label(&labels[0], "dim-label"));
        None
    } else {
        let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
        let scope = gtk::DropDown::from_strings(&labels);
        card.append(&labeled_control(tr("Installation to remove"), &scope));
        Some(scope)
    };

    let (delete_option, delete_data) = crate::flatpak_delete_data_option();
    delete_option.add_css_class("packlift-uninstall-data");
    card.append(&delete_option);

    let dismiss = dismiss_store_options(container, &layout, open_options);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = text_button(tr("Cancel"));
    let close = dismiss.clone();
    cancel.connect_clicked(move |_| close());
    actions.append(&cancel);
    let uninstall = gtk::Button::new();
    uninstall.add_css_class("destructive-action");
    uninstall.add_css_class("pill");
    let confirm_label = home_label(tr("Uninstall"), "");
    confirm_label.set_xalign(0.5);
    confirm_label.set_justify(gtk::Justification::Center);
    uninstall.set_child(Some(&confirm_label));
    actions.append(&uninstall);
    card.append(&actions);
    let data_row = delete_option.downgrade();
    delete_data.connect_toggled(move |check| {
        let deletes_data = check.is_active();
        confirm_label.set_text(if deletes_data {
            tr("Uninstall and delete data")
        } else {
            tr("Uninstall")
        });
        if let Some(row) = data_row.upgrade() {
            if deletes_data {
                row.add_css_class("deleting-data");
            } else {
                row.remove_css_class("deleting-data");
            }
        }
    });

    let keys = gtk::EventControllerKey::new();
    let close = dismiss.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    card.add_controller(keys);

    let parent = window.downgrade();
    let job = progress.downgrade();
    let choice = scope.as_ref().map(|scope| scope.downgrade());
    let delete_data = delete_data.downgrade();
    uninstall.connect_clicked(move |_| {
        let (Some(window), Some(progress)) = (parent.upgrade(), job.upgrade()) else {
            return;
        };
        let index = if let Some(choice) = &choice {
            let Some(scope) = choice.upgrade() else {
                return;
            };
            scope.selected()
        } else {
            0
        };
        let Some(record) = records.get(index as usize).cloned() else {
            return;
        };
        let Some(application) = window.application().and_downcast::<adw::Application>() else {
            return;
        };
        let delete_data = delete_data.upgrade().is_some_and(|check| check.is_active());
        dismiss();
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
    container.append(&layout);
    container.set_visible(true);
    open_options.set_visible(false);
    if let Some(scope) = scope {
        scope.grab_focus();
    } else {
        cancel.grab_focus();
    }
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
    } else {
        caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
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
