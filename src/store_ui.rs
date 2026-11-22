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
    confirm_library_action, home_label, i18n::tr, run_job, show_host_access_dialog, text_button,
};

struct StoreView {
    window: glib::WeakRef<adw::ApplicationWindow>,
    destination: gtk::DropDown,
    source: gtk::DropDown,
    search: gtk::SearchEntry,
    submit: gtk::Button,
    list: gtk::ListBox,
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
    if let Some(window) = app
        .windows()
        .into_iter()
        .find(|window| window.widget_name() == "packlift-store")
    {
        window.present();
        return;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(tr("Store"))
        .default_width(820)
        .default_height(700)
        .build();
    window.set_widget_name("packlift-store");
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

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
    let heading = home_label(tr("Discover apps"), "title-1");
    body.append(&heading);
    let choices = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let source = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let destination = gtk::DropDown::from_strings(&[tr("For my account"), tr("For the system")]);
    choices.append(&labeled_control(tr("Repository"), &source));
    choices.append(&labeled_control(tr("Install scope"), &destination));
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
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
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
    window.set_content(Some(&toolbar));

    let view = Rc::new(StoreView {
        window: window.downgrade(),
        destination,
        source,
        search,
        submit,
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
        if let Some(view) = weak.upgrade() {
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
    window.connect_destroy(move |_| {
        let _ = &keep_alive;
    });
    view.load_remotes();
    window.present();
}

impl StoreView {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.destination.set_sensitive(!busy);
        self.source
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
        self.submit
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
        self.search
            .set_sensitive(!busy && !self.remotes.borrow().is_empty());
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
        self.page_label.set_text("");
        self.status.set_text(tr("Loading repositories…"));
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
                    let titles: Vec<&str> =
                        remotes.iter().map(|remote| remote.title.as_str()).collect();
                    view.source.set_model(Some(&gtk::StringList::new(&titles)));
                    view.source.set_selected(selected as u32);
                    *view.remotes.borrow_mut() = remotes;
                    view.new_search();
                }
                Err(error) => {
                    view.source.set_model(None::<&gtk::StringList>);
                    view.remotes.borrow_mut().clear();
                    view.status.set_text(&error.to_string());
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
        self.page_label.set_text("");
        self.status.set_text(tr("Searching the store…"));
        let query = self.query.borrow().clone();
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
                        view.status.set_text(if results.apps.is_empty() {
                            tr("No apps match this search.")
                        } else {
                            ""
                        });
                        for app in results.apps {
                            let row = adw::ActionRow::builder()
                                .title(&app.name)
                                .subtitle(if app.summary.is_empty() {
                                    &app.id
                                } else {
                                    &app.summary
                                })
                                .title_lines(1)
                                .subtitle_lines(2)
                                .use_markup(false)
                                .activatable(true)
                                .build();
                            row.add_prefix(&gtk::Image::from_icon_name(
                                "application-x-executable-symbolic",
                            ));
                            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                            let chosen = remote.clone();
                            let window = view.window.clone();
                            row.connect_activated(move |_| {
                                if let Some(window) = window.upgrade() {
                                    show_app(&window, &chosen, &app);
                                }
                            });
                            view.list.append(&row);
                        }
                    }
                    Err(error) => view.status.set_text(&error.to_string()),
                }
            },
        );
    }
}

fn show_app(parent: &adw::ApplicationWindow, remote: &Remote, app: &CatalogApp) {
    let window = adw::ApplicationWindow::builder()
        .title(&app.name)
        .transient_for(parent)
        .application(parent.application().as_ref().unwrap())
        .modal(true)
        .default_width(650)
        .default_height(580)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(14);
    body.set_margin_bottom(16);
    body.append(&home_label(&app.name, "title-2"));
    if !app.summary.is_empty() {
        body.append(&home_label(&app.summary, "dim-label"));
    }
    let scope = if remote.destination == Destination::User {
        tr("For my account")
    } else {
        tr("For the system")
    };
    body.append(&home_label(
        &format!("{} · {scope} · {}", remote.title, app.id),
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
    window.set_content(Some(&toolbar));
    let selected = remote.clone();
    let package = app.clone();
    let dialog = window.downgrade();
    let store_window = parent.downgrade();
    install.connect_clicked(move |_| {
        let Some(dialog) = dialog.upgrade() else { return };
        let detail = if selected.name.is_empty() {
            tr("Flathub will be added to the selected installation before installing the app.")
        } else {
            tr("Install this app from the selected repository and scope?")
        };
        let candidate = package.clone();
        let source = selected.clone();
        let store_window = store_window.clone();
        confirm_library_action(&dialog, tr("Install from the store?"), detail, tr("Install"), false,
            move |dialog| {
                let Some(application) = dialog.application().and_downcast::<adw::Application>() else { return };
                let progress = adw::ApplicationWindow::new(&application);
                if let Some(store_window) = store_window.upgrade() {
                    progress.set_transient_for(Some(&store_window));
                }
                progress.set_title(Some(tr("Installing program")));
                let source = source.clone();
                let candidate = candidate.clone();
                run_job(&application, &progress, tr("Installing program"), move |log| {
                    let runner = Runner::new(None, log)?;
                    store::install(&runner, &source, &candidate)?;
                    Ok(Some(json!({"name":candidate.name,"method":"flatpak",
                        "exports":[{"icon":candidate.id,"argv":["flatpak","run",source.destination.flag(),"--",candidate.reference]}]})))
                });
                dialog.close();
                progress.present();
            });
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
    window.present();
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
