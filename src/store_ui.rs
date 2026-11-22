use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use packlift::{
    store::{self, CatalogApp, Destination, Remote, Screenshot},
    system::Runner,
};
use serde_json::json;

use crate::{
    back_to_previous, home_label, i18n::tr, page_navigation, page_shortcut, render_home, run_job,
    show_host_access_dialog, text_button,
};

struct StoreView {
    window: glib::WeakRef<adw::ApplicationWindow>,
    destination: gtk::DropDown,
    source: gtk::DropDown,
    search: gtk::SearchEntry,
    submit: gtk::Button,
    feature: gtk::Box,
    list: gtk::FlowBox,
    status: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    page_label: gtk::Label,
    remotes: RefCell<Vec<Remote>>,
    query: RefCell<String>,
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
    let choices = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let source = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let destination = gtk::DropDown::from_strings(&[tr("For my account"), tr("For the system")]);
    choices.append(&labeled_control(tr("Repository"), &source));
    choices.append(&labeled_control(tr("Show repositories for"), &destination));
    body.append(&choices);

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
    toolbar.set_content(Some(&body));
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
        destination,
        source,
        search,
        submit,
        feature,
        list,
        status,
        previous,
        next,
        page_label,
        remotes: RefCell::new(Vec::new()),
        query: RefCell::new(String::new()),
        page: Cell::new(1),
        busy: Cell::new(false),
    });
    let weak = Rc::downgrade(&view);
    view.destination.connect_selected_notify(move |_| {
        if let Some(view) = weak.upgrade() {
            view.load_remotes();
        }
    });
    let weak = Rc::downgrade(&view);
    view.source.connect_selected_notify(move |_| {
        if let Some(view) = weak.upgrade().filter(|view| !view.busy.get()) {
            view.new_search();
        }
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
        self.destination.set_sensitive(!busy);
        self.source
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
        self.submit
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
        self.search.set_sensitive(!self.remotes.borrow().is_empty());
        if busy {
            self.previous.set_sensitive(false);
            self.next.set_sensitive(false);
        }
    }

    fn load_remotes(self: &Rc<Self>) {
        let previous = self
            .remotes
            .borrow()
            .get(self.source.selected() as usize)
            .cloned();
        self.set_busy(true);
        self.list.remove_all();
        if let Some(child) = self.feature.first_child() {
            self.feature.remove(&child);
        }
        self.page_label.set_text("");
        self.set_status(tr("Loading repositories…"));
        let destination = if self.destination.selected() == 1 {
            Destination::System
        } else {
            Destination::User
        };
        let view = self.clone();
        background(
            move || worker().and_then(|runner| store::remotes(&runner, destination)),
            move |result| match result {
                Ok(remotes) => {
                    let selected = previous
                        .as_ref()
                        .and_then(|previous| {
                            remotes.iter().position(|remote| {
                                remote.url == previous.url || (remote.flathub && previous.flathub)
                            })
                        })
                        .unwrap_or(0);
                    let titles = remotes.iter().map(Remote::label).collect::<Vec<_>>();
                    let labels = titles.iter().map(String::as_str).collect::<Vec<_>>();
                    view.source.set_model(Some(&gtk::StringList::new(&labels)));
                    view.source.set_selected(selected as u32);
                    *view.remotes.borrow_mut() = remotes;
                    view.new_search();
                }
                Err(error) => {
                    view.source.set_model(None::<&gtk::StringList>);
                    view.remotes.borrow_mut().clear();
                    view.set_status(&error.to_string());
                    view.set_busy(false);
                }
            },
        );
    }

    fn new_search(self: &Rc<Self>) {
        *self.query.borrow_mut() = self.search.text().trim().to_owned();
        self.load_page(1);
    }

    fn load_page(self: &Rc<Self>, page: usize) {
        let Some(remote) = self
            .remotes
            .borrow()
            .get(self.source.selected() as usize)
            .cloned()
        else {
            self.set_busy(false);
            return;
        };
        self.set_busy(true);
        self.list.remove_all();
        if let Some(child) = self.feature.first_child() {
            self.feature.remove(&child);
        }
        self.page_label.set_text("");
        self.set_status(tr("Searching the store…"));
        let query = self.query.borrow().clone();
        let explore = query.is_empty() && page == 1;
        let chosen = remote.clone();
        let view = self.clone();
        background(
            move || worker().and_then(|runner| store::search(&runner, &chosen, &query, page)),
            move |result| {
                view.set_busy(false);
                match result {
                    Ok(results) => {
                        view.page.set(page);
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
                            if let Some(app) = apps.next() {
                                view.feature.append(&featured_app(
                                    &view.window,
                                    remote.clone(),
                                    app,
                                ));
                            }
                        }
                        for app in apps {
                            view.list
                                .append(&app_card(&view.window, remote.clone(), app));
                        }
                    }
                    Err(error) => view.set_status(&error.to_string()),
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
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(14);
    body.set_margin_bottom(16);
    body.append(&home_label(&app.name, "title-2"));
    if !app.summary.is_empty() {
        body.append(&home_label(&app.summary, "dim-label"));
    }
    body.append(&home_label(
        &format!("{} · {}", remote.label(), app.id),
        "dim-label",
    ));
    let gallery = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let status = home_label(tr("Loading screenshots…"), "dim-label");
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .height_request(240)
        .build();
    picture.set_visible(false);
    gallery.append(&picture);
    gallery.append(&status);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.set_halign(gtk::Align::Center);
    let previous = text_button(tr("Previous"));
    let next = text_button(tr("Next"));
    controls.append(&previous);
    controls.append(&next);
    controls.set_visible(false);
    gallery.append(&controls);
    body.append(&gallery);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .build();
    toolbar.set_content(Some(&scroll));
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.set_halign(gtk::Align::End);
    footer.set_margin_top(10);
    footer.set_margin_bottom(10);
    footer.set_margin_end(16);
    let install = text_button(tr("Install"));
    install.add_css_class("suggested-action");
    footer.append(&install);
    toolbar.set_bottom_bar_style(adw::ToolbarStyle::RaisedBorder);
    toolbar.add_bottom_bar(&footer);
    window.set_widget_name("packlift-store-details");
    window.set_title(Some(&app.name));
    window.set_content(Some(&toolbar));
    let selected = remote.clone();
    let package = app.clone();
    let dialog = window.downgrade();
    install.connect_clicked(move |button| {
        let Some(dialog) = dialog.upgrade() else {
            return;
        };
        let Some(current_page) = dialog.content() else {
            return;
        };
        let current_page = current_page.downgrade();
        button.set_sensitive(false);
        button.set_label(tr("Loading repositories…"));
        let source = selected.clone();
        let candidate = package.clone();
        let package_name = candidate.name.clone();
        let button = button.downgrade();
        let dialog = dialog.downgrade();
        let preferred = source.destination;
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
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(true);
                    button.set_label(tr("Install"));
                }
                if let Some(dialog) = dialog
                    .upgrade()
                    .filter(|window| window.content().as_ref() == current_page.upgrade().as_ref())
                {
                    choose_install_destination(&dialog, preferred, choices, &package_name);
                }
            },
        );
    });

    let source = remote.clone();
    let selected = app.clone();
    background(
        move || worker().and_then(|runner| store::screenshots(&runner, &source, &selected)),
        move |result| match result {
            Ok(shots) if !shots.is_empty() => {
                bind_gallery(shots, picture, status, controls, previous, next)
            }
            Ok(_) => status.set_text(tr("No screenshots are available from this repository.")),
            Err(_) => status.set_text(tr("Screenshots could not be loaded.")),
        },
    );
    install.grab_focus();
    window.present();
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
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    hero.add_css_class("packlift-store-feature");
    let intro = gtk::Box::new(gtk::Orientation::Vertical, 6);
    intro.set_hexpand(true);
    intro.set_valign(gtk::Align::Center);
    let icon = catalog_icon(&app, 48);
    icon.set_halign(gtk::Align::Start);
    intro.append(&icon);
    let name = home_label(&app.name, "title-2");
    name.set_max_width_chars(24);
    intro.append(&name);
    let summary = home_label(&app.summary, "dim-label");
    summary.set_max_width_chars(30);
    intro.append(&summary);
    let details = text_button(tr("View app"));
    details.set_halign(gtk::Align::Start);
    details.set_margin_top(4);
    details.add_css_class("suggested-action");
    intro.append(&details);
    hero.append(&intro);

    let preview = gtk::Overlay::new();
    preview.set_hexpand(true);
    let background = gtk::Box::new(gtk::Orientation::Vertical, 0);
    background.set_size_request(182, 126);
    preview.set_child(Some(&background));
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    preview.add_overlay(&picture);
    preview.set_visible(false);
    hero.append(&preview);

    let selected = app.clone();
    let origin = remote.clone();
    let image = picture.downgrade();
    let preview = preview.downgrade();
    self::background(
        move || {
            let runner = worker().ok()?;
            let shot = store::screenshots(&runner, &origin, &selected)
                .ok()?
                .into_iter()
                .next()?;
            let bytes = store::screenshot_bytes(&runner, &shot.url).ok()?;
            Some((bytes, shot.caption))
        },
        move |result| {
            let (Some((bytes, caption)), Some(picture), Some(preview)) =
                (result, image.upgrade(), preview.upgrade())
            else {
                return;
            };
            if let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                picture.set_paintable(Some(&texture));
                picture.set_alternative_text(Some(&caption));
                preview.set_visible(true);
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
    preferred: Destination,
    (user, system): (Vec<(Remote, CatalogApp)>, Vec<(Remote, CatalogApp)>),
    name: &str,
) {
    let heading = tr("Where do you want to install {name}?").replace("{name}", name);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(tr("Store"), "")));
    let previous = back_to_previous(&toolbar, &header, window);
    toolbar.add_top_bar(&header);

    let scope = gtk::DropDown::from_strings(&[tr("For my account"), tr("For the system")]);
    scope.set_selected((preferred == Destination::System) as u32);
    let repository = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let info = home_label("", "dim-label");
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 14);
    controls.set_valign(gtk::Align::Start);
    controls.set_margin_top(24);
    controls.set_margin_start(24);
    controls.set_margin_end(24);
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
    let clamp = adw::Clamp::builder()
        .maximum_size(550)
        .child(&controls)
        .build();
    toolbar.set_content(Some(&clamp));

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
            info.set_text(match selected {
                None => tr("No repository offers this app for the selected scope."),
                Some((remote, _)) if remote.flathub && remote.name.is_empty() => {
                    tr("Packlift will add Flathub to the selected scope if needed.")
                }
                _ => "",
            });
        }
    };
    update(&repository);
    repository.connect_selected_notify(update);

    let target = window.downgrade();
    let cancel_page = previous.clone();
    cancel.connect_clicked(move |_| {
        if let (Some(window), Some(page)) = (target.upgrade(), cancel_page.as_ref()) {
            page.restore(&window);
        }
    });
    let target = window.downgrade();
    let weak_scope = scope.downgrade();
    let repository = repository.downgrade();
    install.connect_clicked(move |_| {
        let (Some(window), Some(repository), Some(scope)) =
            (target.upgrade(), repository.upgrade(), weak_scope.upgrade()) else { return; };
        let Some((source, candidate)) = choices[scope.selected().min(1) as usize]
            .get(repository.selected() as usize).cloned() else { return; };
        let Some(application) = window.application().and_downcast::<adw::Application>() else { return; };
        if let Some(page) = &previous {
            page.restore(&window);
        }
        run_job(&application, &window, tr("Installing program"), move |log| {
            let runner = Runner::new(None, log)?;
            store::install(&runner, &source, &candidate)?;
            Ok(Some(json!({"name":candidate.name,"method":"flatpak",
                "exports":[{"icon":candidate.id,"argv":["flatpak","run",source.destination.flag(),"--",candidate.reference]}]})))
        });
    });
    window.set_content(Some(&toolbar));
    scope.grab_focus();
}

fn bind_gallery(
    shots: Vec<Screenshot>,
    picture: gtk::Picture,
    status: gtk::Label,
    controls: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
) {
    controls.set_visible(shots.len() > 1);
    let index = Rc::new(Cell::new(0usize));
    let show = Rc::new({
        let index = index.clone();
        let previous = previous.downgrade();
        let next = next.downgrade();
        move || {
            let i = index.get();
            let Some(shot) = shots.get(i) else { return };
            let (Some(previous), Some(next)) = (previous.upgrade(), next.upgrade()) else {
                return;
            };
            previous.set_sensitive(false);
            next.set_sensitive(false);
            picture.set_visible(false);
            status.set_text(tr("Loading screenshots…"));
            let caption = shot.caption.clone();
            let url = shot.url.clone();
            let picture = picture.clone();
            let status = status.clone();
            let has_next = i + 1 < shots.len();
            background(
                move || worker().and_then(|runner| store::screenshot_bytes(&runner, &url)),
                move |result| {
                    previous.set_sensitive(i > 0);
                    next.set_sensitive(has_next);
                    if let Ok(bytes) = result {
                        if let Ok(texture) =
                            gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
                        {
                            picture.set_paintable(Some(&texture));
                            picture.set_alternative_text(Some(&caption));
                            picture.set_visible(true);
                            status.set_text(&caption);
                            return;
                        }
                    }
                    status.set_text(tr("Screenshots could not be loaded."));
                },
            );
        }
    });
    let back = show.clone();
    let prior_index = index.clone();
    previous.connect_clicked(move |_| {
        prior_index.set(prior_index.get().saturating_sub(1));
        back();
    });
    let forward = show.clone();
    next.connect_clicked(move |_| {
        index.set(index.get() + 1);
        forward();
    });
    show();
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
