use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use packlift::{
    store::{self, CatalogApp, CatalogSearch, Destination, Remote, Screenshot},
    system::Runner,
};
use serde_json::json;

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
    });
    let weak = Rc::downgrade(&view);
    view.search.connect_activate(move |_| {
        if let Some(view) = weak.upgrade().filter(|view| !view.busy.get()) {
            view.new_search();
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
        self.search.set_sensitive(!self.remotes.borrow().is_empty());
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

    let repository_caption = tr("Repository");
    let facts = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    facts.append(&store_fact(repository_caption, &remote.label()));
    hero.append(&facts);
    body.append(&hero);

    let install_options = gtk::Box::new(gtk::Orientation::Vertical, 0);
    install_options.set_visible(false);
    body.append(&install_options);

    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 12);
    gallery.append(&home_label(tr("Screenshots"), "title-3"));
    let gallery_status = home_label(tr("Loading screenshots…"), "dim-label");
    gallery.append(&gallery_status);
    let frames = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    frames.set_homogeneous(true);
    gallery.append(&frames);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.set_halign(gtk::Align::Center);
    let previous = text_button(tr("Previous"));
    let next = text_button(tr("Next"));
    controls.append(&previous);
    controls.append(&next);
    controls.set_visible(false);
    gallery.append(&controls);
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

    let release = gtk::Box::new(gtk::Orientation::Vertical, 8);
    release.add_css_class("packlift-store-release");
    release.set_visible(false);
    body.append(&release);

    let layout = adw::Clamp::builder()
        .maximum_size(960)
        .tightening_threshold(840)
        .child(&body)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&layout)
        .build();
    toolbar.set_content(Some(&scroll));
    window.set_widget_name("packlift-store-details");
    window.set_title(Some(&app.name));
    window.set_content(Some(&toolbar));
    let selected = remote.clone();
    let package = app.clone();
    let dialog = window.downgrade();
    let install_options = install_options.downgrade();
    install.connect_clicked(move |button| {
        let Some(dialog) = dialog.upgrade() else {
            return;
        };
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

    let source = remote.clone();
    let selected = app.clone();
    background(
        move || worker().and_then(|runner| store::app_details(&runner, &source, &selected)),
        move |result| match result {
            Ok(details) => {
                developer.set_text(&details.developer);
                developer.set_visible(!details.developer.is_empty());
                if !details.license.is_empty() {
                    facts.append(&store_fact(tr("License"), &details.license));
                }
                if !details.description.is_empty() {
                    description.set_text(&details.description);
                    description.set_visible(true);
                }
                if let Some(release_info) = details.release {
                    let version =
                        tr("Version {version}").replace("{version}", &release_info.version);
                    release.append(&home_label(&version, "title-3"));
                    if !release_info.notes.is_empty() {
                        let notes = home_label(&release_info.notes, "");
                        notes.set_selectable(true);
                        release.append(&notes);
                    }
                    release.set_visible(true);
                }
                if details.screenshots.is_empty() {
                    gallery_status
                        .set_text(tr("No screenshots are available from this repository."));
                } else {
                    gallery_status.set_visible(false);
                    bind_gallery(details.screenshots, &frames, &controls, &previous, &next);
                }
            }
            Err(_) => gallery_status.set_text(tr("Screenshots could not be loaded.")),
        },
    );
    install.grab_focus();
    window.present();
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
    if let Some(source) = app.icon.clone() {
        let target = image.downgrade();
        let app_id = app.id.clone();
        background(
            move || {
                worker()
                    .ok()
                    .and_then(|runner| store::catalog_icon_bytes(&runner, &source, &app_id))
            },
            move |bytes| {
                let (Some(image), Some(bytes)) = (target.upgrade(), bytes) else {
                    return;
                };
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                    &stream,
                    size,
                    size,
                    true,
                    None::<&gio::Cancellable>,
                ) {
                    image.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&pixbuf)));
                }
            },
        );
    }
    image
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

    let dismiss = Rc::new({
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
    });
    let close = dismiss.clone();
    cancel.connect_clicked(move |_| close());
    let target = window.downgrade();
    let weak_scope = scope.downgrade();
    let repository = repository.downgrade();
    install.connect_clicked(move |_| {
        let (Some(window), Some(repository), Some(scope)) =
            (target.upgrade(), repository.upgrade(), weak_scope.upgrade()) else { return; };
        let Some((source, candidate)) = choices[scope.selected().min(1) as usize]
            .get(repository.selected() as usize).cloned() else { return; };
        let Some(application) = window.application().and_downcast::<adw::Application>() else { return; };
        dismiss();
        run_job(&application, &window, tr("Installing program"), move |log| {
            let runner = Runner::new(None, log)?;
            store::install(&runner, &source, &candidate)?;
            Ok(Some(json!({"name":candidate.name,"method":"flatpak",
                "exports":[{"icon":candidate.id,"argv":["flatpak","run",source.destination.flag(),"--",candidate.reference]}]})))
        });
    });
    container.append(&controls);
    container.set_visible(true);
    open_options.set_visible(false);
    scope.grab_focus();
}

fn bind_gallery(
    shots: Vec<Screenshot>,
    frames: &gtk::Box,
    controls: &gtk::Box,
    previous: &gtk::Button,
    next: &gtk::Button,
) {
    const PER_VIEW: usize = 2;
    controls.set_visible(shots.len() > PER_VIEW);
    let index = Rc::new(Cell::new(0usize));
    let show = Rc::new({
        let previous = previous.downgrade();
        let next = next.downgrade();
        let frames = frames.downgrade();
        move |start: usize| {
            let (Some(frames), Some(previous), Some(next)) =
                (frames.upgrade(), previous.upgrade(), next.upgrade())
            else {
                return;
            };
            while let Some(child) = frames.first_child() {
                frames.remove(&child);
            }
            previous.set_sensitive(start > 0);
            next.set_sensitive(start + PER_VIEW < shots.len());
            for shot in shots.iter().skip(start).take(PER_VIEW) {
                frames.append(&screenshot_frame(shot));
            }
        }
    });
    let back = show.clone();
    let prior_index = index.clone();
    let forward_button = next.downgrade();
    previous.connect_clicked(move |button| {
        let start = prior_index.get().saturating_sub(PER_VIEW);
        prior_index.set(start);
        let focused = button.has_focus();
        back(start);
        if focused && !button.is_sensitive() {
            if let Some(next) = forward_button.upgrade() {
                next.grab_focus();
            }
        }
    });
    let forward = show.clone();
    let back_button = previous.downgrade();
    next.connect_clicked(move |button| {
        let start = index.get() + PER_VIEW;
        index.set(start);
        let focused = button.has_focus();
        forward(start);
        if focused && !button.is_sensitive() {
            if let Some(previous) = back_button.upgrade() {
                previous.grab_focus();
            }
        }
    });
    show(0);
}

fn screenshot_frame(shot: &Screenshot) -> gtk::Box {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 8);
    frame.add_css_class("packlift-store-shot");
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .height_request(228)
        .build();
    if !shot.caption.is_empty() {
        picture.set_alternative_text(Some(&shot.caption));
    }
    frame.append(&picture);
    let caption = home_label(tr("Loading screenshots…"), "dim-label");
    frame.append(&caption);
    let url = shot.url.clone();
    let text = shot.caption.clone();
    let target = frame.downgrade();
    background(
        move || worker().and_then(|runner| store::screenshot_bytes(&runner, &url)),
        move |result| {
            if target.upgrade().is_none() {
                return;
            }
            if let Ok(bytes) = result {
                if let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
                {
                    picture.set_paintable(Some(&texture));
                    caption.set_text(&text);
                    caption.set_visible(!text.is_empty());
                    return;
                }
            }
            picture.set_visible(false);
            caption.set_text(tr("Screenshots could not be loaded."));
        },
    );
    frame
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
