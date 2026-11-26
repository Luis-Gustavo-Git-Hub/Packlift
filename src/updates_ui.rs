use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    fs,
    rc::Rc,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use adw::prelude::*;
use packlift::{
    core::{PackliftError, Result},
    system::{host_access_available, operation, packlift_data_dir, Runner},
};

use crate::{home_label, i18n::tr, text_button};

#[derive(Clone, Copy)]
enum Source {
    System,
    Flatpaks,
}

#[derive(Clone)]
enum Backend {
    PackageKit,
    Ostree,
    Apt,
    Dnf,
    Flatpak(&'static str, Vec<String>),
    Flatpaks(Vec<(&'static str, Vec<String>)>),
}

struct Available {
    backend: Backend,
    available: bool,
    packages: Vec<String>,
    failed_remotes: Vec<String>,
}

fn command(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

const UPDATE_INTERVAL_DAYS: [u64; 4] = [0, 1, 7, 30];

thread_local! {
    static AUTO_UPDATE_TIMER_STARTED: Cell<bool> = const { Cell::new(false) };
    static AUTO_UPDATE_RUNNING: Cell<bool> = const { Cell::new(false) };
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn schedule_path() -> std::path::PathBuf {
    packlift_data_dir().join("updates.json")
}

fn read_schedule() -> (u64, u64) {
    let contents = fs::read_to_string(schedule_path()).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&contents).unwrap_or_default();
    let days = value["interval_days"]
        .as_u64()
        .filter(|days| UPDATE_INTERVAL_DAYS.contains(days))
        .unwrap_or(1);
    let last_run = value["last_auto_update"].as_u64().unwrap_or(0);
    (days, last_run)
}

fn write_schedule(days: u64, last_run: u64) {
    let path = schedule_path();
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::json!({
        "interval_days": days,
        "last_auto_update": last_run,
    });
    if fs::write(&temporary, contents.to_string()).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

fn distribution_identity() -> (String, String) {
    let result = Runner::new(None, Arc::new(|_| {}))
        .and_then(|runner| runner.capture(&command(&["cat", "/etc/os-release"]), false));
    let Ok(contents) = result else {
        return (tr("Linux system").to_owned(), "computer-symbolic".to_owned());
    };
    let values: BTreeMap<_, _> = contents
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key, value.trim_matches('"')))
        .collect();
    let name = values
        .get("PRETTY_NAME")
        .copied()
        .unwrap_or("Linux system")
        .to_owned();
    let candidate = values
        .get("LOGO")
        .map(|logo| (*logo).to_owned())
        .or_else(|| values.get("ID").map(|id| format!("distributor-logo-{id}")))
        .unwrap_or_else(|| "computer-symbolic".to_owned());
    (name, candidate)
}

fn apply_due_updates() -> Result<()> {
    let mut errors = Vec::new();
    for source in [
        Source::System,
        Source::Flatpaks,
    ] {
        match check(source) {
            Ok(available) if available.available => {
                if let Err(error) = apply(&available.backend) {
                    errors.push(error.to_string());
                }
            }
            Ok(_) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PackliftError::Message(errors.join("\n")))
    }
}

fn ensure_auto_update_timer(app: &gtk::Application) {
    let started = AUTO_UPDATE_TIMER_STARTED.with(|started| started.replace(true));
    if started {
        return;
    }
    let (days, last_run) = read_schedule();
    if last_run == 0 {
        // The update sections check immediately; start the automatic schedule from now.
        write_schedule(days, now_epoch());
    }
    let app = app.downgrade();
    glib::timeout_add_seconds_local(60, move || {
        let Some(app) = app.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !host_access_available() || AUTO_UPDATE_RUNNING.with(Cell::get) {
            return glib::ControlFlow::Continue;
        }
        let (days, last_run) = read_schedule();
        let now = now_epoch();
        if last_run == 0 {
            write_schedule(days, now);
            return glib::ControlFlow::Continue;
        }
        if days == 0 || now.saturating_sub(last_run) < days.saturating_mul(24 * 60 * 60) {
            return glib::ControlFlow::Continue;
        }
        write_schedule(days, now);
        AUTO_UPDATE_RUNNING.with(|running| running.set(true));
        let hold = app.hold();
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let _ = gio::spawn_blocking(apply_due_updates).await;
            AUTO_UPDATE_RUNNING.with(|running| running.set(false));
        });
        glib::ControlFlow::Continue
    });
}

fn check(source: Source) -> Result<Available> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    let (backend, available, packages) = match source {
        Source::Flatpaks => return check_all_flatpaks(&runner),
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
        failed_remotes: Vec::new(),
    })
}

fn check_flatpaks(runner: &Runner, scope: &'static str) -> Result<Available> {
    let installed = runner.capture(
        &command(&["flatpak", "list", scope, "--columns=origin,ref"]),
        false,
    )?;
    let mut origins = BTreeMap::<&str, BTreeSet<&str>>::new();
    for line in installed.lines() {
        let mut columns = line.split_whitespace();
        if let (Some(origin), Some(reference)) = (columns.next(), columns.next()) {
            origins
                .entry(origin)
                .or_default()
                .insert(flatpak_ref(reference));
        }
    }
    let mut updates = BTreeSet::new();
    let mut failed_remotes = Vec::new();
    for (origin, installed) in origins {
        match runner.capture(
            &command(&[
                "flatpak",
                "remote-ls",
                scope,
                "--updates",
                "--columns=ref",
                "--",
                origin,
            ]),
            false,
        ) {
            Ok(output) => {
                updates.extend(
                    output
                        .split_whitespace()
                        .map(flatpak_ref)
                        .filter(|reference| installed.contains(reference))
                        .map(str::to_owned),
                );
            }
            Err(_) => failed_remotes.push(origin.to_owned()),
        }
    }
    let packages = updates
        .iter()
        .filter_map(|reference| reference.split('/').next())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(Available {
        available: !updates.is_empty(),
        backend: Backend::Flatpak(scope, updates.into_iter().collect()),
        packages,
        failed_remotes,
    })
}

fn check_all_flatpaks(runner: &Runner) -> Result<Available> {
    let mut scopes = Vec::new();
    let mut packages = BTreeSet::new();
    let mut failed_remotes = Vec::new();
    let mut errors = Vec::new();
    for scope in ["--user", "--system"] {
        match check_flatpaks(runner, scope) {
            Ok(result) => {
                packages.extend(result.packages);
                failed_remotes.extend(
                    result
                        .failed_remotes
                        .into_iter()
                        .map(|remote| format!("{scope}: {remote}")),
                );
                if let Backend::Flatpak(scope, refs) = result.backend {
                    scopes.push((scope, refs));
                }
            }
            Err(error) => errors.push(format!("{scope}: {error}")),
        }
    }
    failed_remotes.extend(errors);
    if scopes.is_empty() {
        return Err(PackliftError::Message(
            failed_remotes.join("\n"),
        ));
    }
    let available = scopes.iter().any(|(_, refs)| !refs.is_empty());
    Ok(Available {
        backend: Backend::Flatpaks(scopes),
        available,
        packages: packages.into_iter().collect(),
        failed_remotes,
    })
}

fn flatpak_ref(reference: &str) -> &str {
    reference
        .strip_prefix("app/")
        .or_else(|| reference.strip_prefix("runtime/"))
        .unwrap_or(reference)
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

fn apply(backend: &Backend) -> Result<()> {
    let _operation = operation(&packlift_data_dir())?;
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    match backend {
        Backend::PackageKit => {
            runner.run(
                &command(&["pkcon", "--plain", "--noninteractive", "update"]),
                false,
                true,
                true,
            )?;
        }
        Backend::Ostree => {
            runner.run(&command(&["rpm-ostree", "upgrade"]), false, true, true)?;
        }
        Backend::Apt => {
            runner.run(&command(&["pkexec", "apt-get", "-y", "upgrade"]), false, true, true)?;
        }
        Backend::Dnf => {
            runner.run(&command(&["pkexec", "dnf", "-y", "upgrade"]), false, true, true)?;
        }
        Backend::Flatpak(scope, refs) => {
            let mut args = command(&["flatpak", "update", scope, "--assumeyes", "--"]);
            args.extend(refs.iter().cloned());
            runner.run(&args, false, true, true)?;
        }
        Backend::Flatpaks(scopes) => {
            for (scope, refs) in scopes.iter().filter(|(_, refs)| !refs.is_empty()) {
                let mut args = command(&["flatpak", "update", scope, "--assumeyes", "--"]);
                args.extend(refs.iter().cloned());
                runner.run(&args, false, true, true)?;
            }
        }
    }
    Ok(())
}

struct Section {
    source: Source,
    status: gtk::Label,
    details: gtk::Label,
    warning: gtk::Label,
    update: gtk::Button,
    busy: Cell<bool>,
    backend: RefCell<Option<Backend>>,
}

impl Section {
    fn new(
        source: Source,
        title: &str,
        icon_name: &str,
        panel: &gtk::Box,
        window: &adw::ApplicationWindow,
    ) -> Rc<Self> {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 8);
        group.add_css_class(if matches!(source, Source::System) {
            "packlift-update-system"
        } else {
            "packlift-update-section"
        });
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::from_icon_name(icon_name);
        icon.set_pixel_size(18);
        heading.append(&icon);
        heading.append(&home_label(title, "heading"));
        group.append(&heading);
        let status = home_label(tr("Waiting to check for updates."), "dim-label");
        group.append(&status);
        let details = home_label("", "caption");
        details.set_selectable(true);
        details.set_visible(false);
        group.append(&details);
        let warning = home_label("", "dim-label");
        warning.set_visible(false);
        group.append(&warning);
        let update = text_button(tr("Update"));
        update.add_css_class("suggested-action");
        update.set_halign(gtk::Align::Fill);
        update.set_hexpand(true);
        update.set_margin_top(4);
        update.set_visible(false);
        group.append(&update);
        panel.append(&group);
        let section = Rc::new(Self {
            source,
            status,
            details,
            warning,
            update,
            busy: Cell::new(false),
            backend: RefCell::new(None),
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
        self.warning.set_visible(false);
        self.status.set_text(tr("Checking for updates…"));
        let section = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let source = section.source;
            let result = gio::spawn_blocking(move || check(source)).await;
            section.busy.set(false);
            match result {
                Ok(Ok(result)) => {
                    *section.backend.borrow_mut() = result.available.then_some(result.backend);
                    let partial = !result.failed_remotes.is_empty();
                    section.warning.set_visible(partial);
                    if partial {
                        section.warning.set_text(
                            &tr("Could not check these Flatpak repositories: {repositories}")
                                .replace("{repositories}", &result.failed_remotes.join(", ")),
                        );
                    }
                    section.status.set_text(if result.available {
                        tr("Updates available")
                    } else if partial {
                        tr("Update check incomplete")
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
                    *section.backend.borrow_mut() = None;
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
        let Some(backend) = self.backend.borrow().clone() else {
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
            let restart = matches!(backend, Backend::Ostree);
            let result = gio::spawn_blocking(move || apply(&backend)).await;
            section.busy.set(false);
            section.update.set_sensitive(true);
            match result {
                Ok(Ok(())) => {
                    *section.backend.borrow_mut() = None;
                    section.update.set_visible(false);
                    section.details.set_visible(false);
                    section.status.set_text(if restart {
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
    split.set_min_sidebar_width(390.0);
    split.set_max_sidebar_width(520.0);
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
    let title = home_label(tr("Updates"), "title-2");
    sidebar.append(&title);

    let distro_module = gtk::Box::new(gtk::Orientation::Vertical, 14);
    distro_module.add_css_class("packlift-update-module");
    let distribution = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    distribution.add_css_class("packlift-update-hero");
    let distro_icon = gtk::Image::from_icon_name("computer-symbolic");
    distro_icon.set_pixel_size(42);
    distribution.append(&distro_icon);
    let distro_name = home_label(tr("Linux system"), "title-2");
    distro_name.set_hexpand(true);
    distro_name.set_valign(gtk::Align::Center);
    distribution.append(&distro_name);
    let refresh = text_button(tr("Check for updates"));
    refresh.add_css_class("suggested-action");
    distribution.append(&refresh);
    distro_module.append(&distribution);
    let distro_icon = distro_icon.downgrade();
    let distro_name = distro_name.downgrade();
    glib::MainContext::default().spawn_local(async move {
        if let Ok((name, icon)) = gio::spawn_blocking(distribution_identity).await {
            if let Some(label) = distro_name.upgrade() {
                label.set_text(&name);
            }
            if let Some(image) = distro_icon.upgrade() {
                let icon = gtk::gdk::Display::default()
                    .filter(|display| gtk::IconTheme::for_display(display).has_icon(&icon))
                    .map_or("computer-symbolic", |_| icon.as_str());
                image.set_icon_name(Some(icon));
            }
        }
    });
    let system = Section::new(
        Source::System,
        tr("System"),
        "system-software-update-symbolic",
        &distro_module,
        window,
    );
    distro_module.append(&home_label(tr("More options"), "title-3"));

    let preferences = gtk::Box::new(gtk::Orientation::Vertical, 8);
    preferences.add_css_class("packlift-update-preferences");
    let automatic_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let automatic_text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let automatic_title = home_label(tr("Install updates automatically"), "heading");
    automatic_text.append(&automatic_title);
    let helper = home_label(
        tr("Packlift checks when it opens and applies updates on this schedule while it is running."),
        "dim-label",
    );
    helper.set_wrap(true);
    automatic_text.append(&helper);
    automatic_row.append(&automatic_text);
    let automatic = gtk::Switch::new();
    automatic.set_valign(gtk::Align::Center);
    let (days, _) = read_schedule();
    automatic.set_active(days > 0);
    automatic.update_property(&[gtk::accessible::Property::Label(tr("Install updates automatically"))]);
    automatic_row.append(&automatic);
    preferences.append(&automatic_row);

    let frequency_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    frequency_row.append(&home_label(tr("Frequency"), ""));
    let frequency_model = gtk::StringList::new(&[&tr("Daily"), &tr("Weekly"), &tr("Monthly")]);
    let frequency = gtk::DropDown::new(Some(frequency_model), None::<gtk::Expression>);
    frequency.set_hexpand(true);
    frequency.set_selected(match days {
        7 => 1,
        30 => 2,
        _ => 0,
    });
    frequency.set_sensitive(days > 0);
    frequency.update_property(&[gtk::accessible::Property::Label(tr("Automatic update frequency"))]);
    frequency_row.append(&frequency);
    preferences.append(&frequency_row);
    distro_module.append(&preferences);
    sidebar.append(&distro_module);

    let flatpaks = Section::new(
        Source::Flatpaks,
        tr("Flatpak"),
        "package-x-generic-symbolic",
        &sidebar,
        window,
    );
    let sections = [system, flatpaks];
    let frequency_target = frequency.downgrade();
    automatic.connect_active_notify(move |automatic| {
        let Some(frequency) = frequency_target.upgrade() else {
            return;
        };
        frequency.set_sensitive(automatic.is_active());
        let interval = match frequency.selected() {
            1 => 7,
            2 => 30,
            _ => 1,
        };
        write_schedule(if automatic.is_active() { interval } else { 0 }, now_epoch());
    });
    let automatic_target = automatic.downgrade();
    frequency.connect_selected_notify(move |frequency| {
        let interval = match frequency.selected() {
            1 => 7,
            2 => 30,
            _ => 1,
        };
        let enabled = automatic_target.upgrade().is_some_and(|automatic| automatic.is_active());
        write_schedule(if enabled { interval } else { 0 }, now_epoch());
    });
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
    if let Some(app) = window.application() {
        ensure_auto_update_timer(&app);
    }
    refresh_all();
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
