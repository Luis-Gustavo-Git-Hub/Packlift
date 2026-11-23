use std::{cell::Cell, rc::Rc, sync::Arc};

use adw::prelude::*;
use packlift::{
    core::{PackliftError, Result},
    system::{host_access_available, operation, packlift_data_dir, Runner},
};

use crate::{home_label, i18n::tr, text_button};

#[derive(Clone, Copy)]
enum Source {
    System,
    Flatpak(&'static str),
}

#[derive(Clone, Copy)]
enum Backend {
    PackageKit,
    Ostree,
    Apt,
    Dnf,
    Flatpak(&'static str),
}

struct Available {
    backend: Backend,
    available: bool,
    packages: Vec<String>,
}

fn command(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn check(source: Source) -> Result<Available> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    let (backend, available, packages) = match source {
        Source::Flatpak(scope) => {
            let output = runner.capture(
                &command(&[
                    "flatpak",
                    "remote-ls",
                    scope,
                    "--updates",
                    "--columns=application",
                ]),
                false,
            )?;
            let mut packages: Vec<_> = output.split_whitespace().map(str::to_owned).collect();
            packages.sort_unstable();
            packages.dedup();
            (Backend::Flatpak(scope), !packages.is_empty(), packages)
        }
        Source::System => {
            let ostree = runner.available("rpm-ostree")
                && runner
                    .run(
                        &command(&["test", "-e", "/run/ostree-booted"]),
                        false,
                        true,
                        false,
                    )?
                    .status
                    .success();
            if ostree {
                let available = check_exit(&runner, &["rpm-ostree", "upgrade", "--check"], 0, 77)?;
                (Backend::Ostree, available, Vec::new())
            } else if runner.available("pkcon") {
                let available = check_exit(&runner, &["pkcon", "--plain", "get-updates"], 0, 5)?;
                (Backend::PackageKit, available, Vec::new())
            } else if runner.available("apt-get") && runner.available("pkexec") {
                let output = runner.capture(
                    &command(&["env", "LC_ALL=C", "apt-get", "-s", "upgrade"]),
                    false,
                )?;
                let packages: Vec<_> = output
                    .lines()
                    .filter_map(|line| line.strip_prefix("Inst ")?.split_whitespace().next())
                    .map(str::to_owned)
                    .collect();
                (Backend::Apt, !packages.is_empty(), packages)
            } else if runner.available("dnf") && runner.available("pkexec") {
                let available = check_exit(&runner, &["dnf", "check-update"], 100, 0)?;
                (Backend::Dnf, available, Vec::new())
            } else {
                return Err(PackliftError::Message(
                    tr("System updates are unavailable on this distribution.").into(),
                ));
            }
        }
    };
    Ok(Available {
        backend,
        available,
        packages,
    })
}

fn check_exit(runner: &Runner, args: &[&str], available: i32, current: i32) -> Result<bool> {
    let result = runner.run(&command(args), false, true, false)?;
    match result.status.code() {
        Some(code) if code == available => Ok(true),
        Some(code) if code == current => Ok(false),
        _ => Err(PackliftError::Message(
            tr("Could not check for updates. See the Packlift log.").into(),
        )),
    }
}

fn apply(backend: Backend) -> Result<()> {
    let _operation = operation(&packlift_data_dir())?;
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    let args: &[&str] = match backend {
        Backend::PackageKit => &["pkcon", "--plain", "--noninteractive", "update"],
        Backend::Ostree => &["rpm-ostree", "upgrade"],
        Backend::Apt => &["pkexec", "apt-get", "-y", "upgrade"],
        Backend::Dnf => &["pkexec", "dnf", "-y", "upgrade"],
        Backend::Flatpak(scope) => &["flatpak", "update", scope, "--assumeyes"],
    };
    runner.run(&command(args), false, true, true)?;
    Ok(())
}

struct Section {
    source: Source,
    status: gtk::Label,
    details: gtk::Label,
    update: gtk::Button,
    busy: Cell<bool>,
    backend: Cell<Option<Backend>>,
}

impl Section {
    fn new(
        source: Source,
        title: &str,
        panel: &gtk::Box,
        window: &adw::ApplicationWindow,
    ) -> Rc<Self> {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 8);
        group.add_css_class("packlift-update-section");
        group.append(&home_label(title, "heading"));
        let status = home_label(tr("Waiting to check for updates."), "dim-label");
        group.append(&status);
        let details = home_label("", "caption");
        details.set_selectable(true);
        details.set_visible(false);
        group.append(&details);
        let update = text_button(tr("Update"));
        update.add_css_class("suggested-action");
        update.set_halign(gtk::Align::Start);
        update.set_visible(false);
        group.append(&update);
        panel.append(&group);
        let section = Rc::new(Self {
            source,
            status,
            details,
            update,
            busy: Cell::new(false),
            backend: Cell::new(None),
        });
        let weak = Rc::downgrade(&section);
        let app = window.application().map(|app| app.downgrade());
        section.update.connect_clicked(move |_| {
            let (Some(section), Some(app)) = (
                weak.upgrade(),
                app.as_ref().and_then(glib::WeakRef::upgrade),
            ) else {
                return;
            };
            section.start_update(&app);
        });
        section
    }

    fn refresh(self: &Rc<Self>) {
        if self.busy.replace(true) {
            return;
        }
        self.update.set_visible(false);
        self.details.set_visible(false);
        self.status.set_text(tr("Checking for updates…"));
        let section = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let source = section.source;
            let result = gio::spawn_blocking(move || check(source)).await;
            section.busy.set(false);
            match result {
                Ok(Ok(result)) => {
                    section
                        .backend
                        .set(result.available.then_some(result.backend));
                    section.status.set_text(if result.available {
                        tr("Updates available")
                    } else {
                        tr("Up to date")
                    });
                    let mut lines = result.packages.iter().take(8).cloned().collect::<Vec<_>>();
                    if result.packages.len() > 8 {
                        lines.push(
                            tr("And {count} more")
                                .replace("{count}", &(result.packages.len() - 8).to_string()),
                        );
                    }
                    section.details.set_text(&lines.join("\n"));
                    section.details.set_visible(!lines.is_empty());
                    section.update.set_visible(result.available);
                }
                Ok(Err(error)) => {
                    section.backend.set(None);
                    if matches!(error, PackliftError::HostAccessRequired) {
                        section
                            .status
                            .set_text(tr("System access is required to check for updates."));
                    } else {
                        section.status.set_text(&error.to_string());
                    }
                }
                Err(_) => section
                    .status
                    .set_text(tr("Could not check for updates. See the Packlift log.")),
            }
        });
    }

    fn start_update(self: &Rc<Self>, app: &gtk::Application) {
        let Some(backend) = self.backend.get() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.status.set_text(tr("Applying updates…"));
        self.update.set_sensitive(false);
        let hold = app.hold();
        let section = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let result = gio::spawn_blocking(move || apply(backend)).await;
            section.busy.set(false);
            section.update.set_sensitive(true);
            match result {
                Ok(Ok(())) => {
                    section.backend.set(None);
                    section.update.set_visible(false);
                    section.details.set_visible(false);
                    section
                        .status
                        .set_text(if matches!(backend, Backend::Ostree) {
                            tr("Updates prepared. Restart to finish.")
                        } else {
                            tr("Updates installed.")
                        });
                }
                Ok(Err(error)) => section.status.set_text(&error.to_string()),
                Err(_) => section
                    .status
                    .set_text(tr("The operation ended unexpectedly.")),
            }
        });
    }
}

pub(super) fn attach(
    toolbar: &adw::ToolbarView,
    header: &adw::HeaderBar,
    window: &adw::ApplicationWindow,
) {
    let Some(content) = toolbar.content() else {
        return;
    };
    toolbar.set_content(None::<&gtk::Widget>);
    let split = adw::OverlaySplitView::new();
    split.set_content(Some(&content));
    split.set_sidebar_position(gtk::PackType::End);
    split.set_min_sidebar_width(280.0);
    split.set_max_sidebar_width(370.0);
    split.set_collapsed(true);
    split.set_show_sidebar(false);
    toolbar.set_content(Some(&split));

    let button = gtk::ToggleButton::new();
    button.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("software-update-available-symbolic")
            .label(tr("Updates"))
            .build(),
    ));
    header.pack_end(&button);
    button
        .bind_property("active", &split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_margin_top(16);
    sidebar.set_margin_bottom(16);
    sidebar.set_margin_start(16);
    sidebar.set_margin_end(16);
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = home_label(tr("Updates"), "title-2");
    title.set_hexpand(true);
    title_row.append(&title);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(tr("Check for updates")));
    refresh.update_property(&[gtk::accessible::Property::Label(tr("Check for updates"))]);
    title_row.append(&refresh);
    sidebar.append(&title_row);
    let sections = [
        Section::new(Source::System, tr("System"), &sidebar, window),
        Section::new(
            Source::Flatpak("--user"),
            tr("Flatpaks for my account"),
            &sidebar,
            window,
        ),
        Section::new(
            Source::Flatpak("--system"),
            tr("System Flatpaks"),
            &sidebar,
            window,
        ),
    ];
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&sidebar)
        .build();
    split.set_sidebar(Some(&scroll));

    let refresh_all = move || {
        for section in &sections {
            section.refresh();
        }
    };
    let manual = refresh_all.clone();
    refresh.connect_clicked(move |_| manual());
    split.connect_show_sidebar_notify(move |split| {
        if split.shows_sidebar() {
            refresh_all();
        }
    });
    let panel = split.downgrade();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(panel) = panel.upgrade().filter(|panel| panel.shows_sidebar()) {
                panel.set_show_sidebar(false);
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    toolbar.add_controller(keys);
}
