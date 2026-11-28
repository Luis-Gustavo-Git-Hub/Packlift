use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    fs,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use adw::prelude::*;
use packlift::{
    core::{PackliftError, Result},
    installer::{managed_appimage_path, normalize_appimage_source},
    system::{home_dir, host_access_available, operation, packlift_data_dir, LogHandler, Runner},
};

use crate::{home_label, i18n::tr, operations_ui, text_button};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    System,
    Flatpaks,
    AppImages,
}

#[derive(Clone, Copy)]
enum SystemUpdater {
    RpmOstree,
    PackageKit,
}

#[derive(Clone)]
enum Backend {
    System(SystemUpdater),
    Flatpak(&'static str, Vec<String>),
    Flatpaks(Vec<(&'static str, Vec<String>)>),
    AppImages(String, Vec<AppImageUpdate>),
}

#[derive(Clone)]
struct AppImageUpdate {
    id: String,
    path: std::path::PathBuf,
    source: Option<String>,
}

impl AppImageUpdate {
    fn command(&self, updater: &str, mode: &str) -> Vec<String> {
        let mut args = command(&[updater, mode]);
        if let Some(source) = &self.source {
            args.push(format!("--update-info={source}"));
        }
        args.extend(["--".into(), self.path.to_string_lossy().into_owned()]);
        args
    }
}

struct Available {
    backend: Backend,
    available: bool,
    packages: Vec<String>,
    runtime_updates: usize,
    failed_remotes: Vec<String>,
    unsupported: usize,
    supported: usize,
}

type CheckResult = std::result::Result<Available, String>;

#[derive(Default)]
struct CheckState {
    cached: Option<(Instant, Rc<CheckResult>, String)>,
    waiting: Vec<std::rc::Weak<Section>>,
    invalidated: bool,
}

fn invalidate_check(source: Source) {
    CHECKS.with_borrow_mut(|checks| {
        if let Some(state) = checks.get_mut(&source) {
            state.cached = None;
            state.invalidated = !state.waiting.is_empty();
        }
    });
}

pub(super) fn appimage_settings(
    window: &adw::ApplicationWindow,
    record: &serde_json::Value,
    saved: impl Fn(serde_json::Value) + 'static,
) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let Some(id) = record["id"].as_str().map(str::to_owned) else {
        return panel;
    };
    let Some(path) = managed_appimage_path(&packlift_data_dir(), &id) else {
        return panel;
    };
    let options = adw::PreferencesGroup::new();
    let source = adw::EntryRow::builder()
        .title(tr("AppImage update source"))
        .build();
    let stored = record["appimage_update_source"]
        .as_str()
        .unwrap_or_default();
    source.set_text(stored.strip_prefix("zsync|").unwrap_or(stored));
    options.add(&source);
    panel.append(&options);
    panel.append(&home_label(
        tr("Use a .zsync link from the publisher. Leave blank to use the AppImage's own source."),
        "dim-label",
    ));
    let status = home_label("", "dim-label");
    status.set_visible(false);
    panel.append(&status);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let save = text_button(tr("Save"));
    let folder = text_button(tr("Show AppImage in folder"));
    actions.append(&save);
    actions.append(&folder);
    panel.append(&actions);
    let parent = window.downgrade();
    folder.connect_clicked(move |_| {
        let Some(window) = parent.upgrade() else {
            return;
        };
        let parent = window.downgrade();
        gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).open_containing_folder(
            Some(&window),
            gio::Cancellable::NONE,
            move |result| {
                if let (Err(error), Some(window)) = (result, parent.upgrade()) {
                    crate::show_error(&window, &error.to_string());
                }
            },
        );
    });
    let source = source.downgrade();
    let status = status.downgrade();
    let saved = Rc::new(saved);
    let application = window.application().map(|app| app.downgrade());
    save.connect_clicked(move |button| {
        let (Some(source), Some(status)) = (source.upgrade(), status.upgrade()) else {
            return;
        };
        let input = source.text().to_string();
        button.set_sensitive(false);
        source.set_sensitive(false);
        status.set_visible(false);
        let (button, source, status) = (button.downgrade(), source.downgrade(), status.downgrade());
        let (id, saved) = (id.clone(), saved.clone());
        let hold = application
            .as_ref()
            .and_then(glib::WeakRef::upgrade)
            .map(|app| app.hold());
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let result = gio::spawn_blocking(move || {
                crate::installer()?.set_appimage_update_source(&id, &input)
            })
            .await
            .unwrap_or_else(|_| {
                Err(PackliftError::Message(
                    tr("The operation ended unexpectedly.").into(),
                ))
            });
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
            if let Some(source) = source.upgrade() {
                source.set_sensitive(true);
            }
            let message = match result {
                Ok(record) => {
                    invalidate_check(Source::AppImages);
                    saved(record);
                    tr("Update source saved.").to_owned()
                }
                Err(error) => match error.to_string().as_str() {
                    "Use an HTTPS .zsync link or a supported release update string." => {
                        tr("Use an HTTPS .zsync link or a supported release update string.").into()
                    }
                    message => message.to_owned(),
                },
            };
            if let Some(status) = status.upgrade() {
                status.set_text(&message);
                status.set_visible(true);
            }
        });
    });
    panel
}

fn command(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn system_command(args: &[&str]) -> Vec<String> {
    ["env", "LC_ALL=C.UTF-8"]
        .into_iter()
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

const UPDATE_INTERVAL_DAYS: [u64; 4] = [0, 1, 7, 30];

thread_local! {
    static AUTO_UPDATE_TIMER_STARTED: Cell<bool> = const { Cell::new(false) };
    static AUTO_CHECK_TIMER_STARTED: Cell<bool> = const { Cell::new(false) };
    static BACKGROUND_MODE_STARTED: Cell<bool> = const { Cell::new(false) };
    static BACKGROUND_SYSTEM_CHECK_RUNNING: Cell<bool> = const { Cell::new(false) };
    static AUTO_RETRY: RefCell<BTreeMap<Source, Instant>> = const { RefCell::new(BTreeMap::new()) };
    static CHECKS: RefCell<BTreeMap<Source, CheckState>> = const { RefCell::new(BTreeMap::new()) };
    static UPDATE_SECTIONS: RefCell<Vec<std::rc::Weak<Section>>> = const { RefCell::new(Vec::new()) };
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn automatic_updates_can_use_network() -> bool {
    let network = gio::NetworkMonitor::default();
    network.is_network_available() && !network.is_network_metered()
}

fn running_on_battery() -> bool {
    let Ok(supplies) = fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let mut has_battery = false;
    let mut has_external_power = false;
    let mut external_power_connected = false;
    for supply in supplies.flatten() {
        let path = supply.path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim().eq_ignore_ascii_case("battery") {
            has_battery = true;
        } else if fs::read_to_string(path.join("online")).is_ok_and(|online| online.trim() == "1") {
            has_external_power = true;
            external_power_connected = true;
        } else if path.join("online").exists() {
            has_external_power = true;
        }
    }
    has_battery && has_external_power && !external_power_connected
}

fn schedule_path() -> std::path::PathBuf {
    packlift_data_dir().join("updates.json")
}

fn autostart_path() -> std::path::PathBuf {
    home_dir()
        .join(".config")
        .join("autostart")
        .join("io.github.luis_gustavo_git_hub.Packlift-background.desktop")
}

fn autostart_disabled_marker() -> std::path::PathBuf {
    home_dir()
        .join(".config")
        .join("packlift")
        .join("disable-autostart")
}

fn start_with_system_enabled() -> bool {
    !autostart_disabled_marker().exists()
}

fn set_start_with_system(enabled: bool) {
    let marker = autostart_disabled_marker();
    if let Some(parent) = marker.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if enabled {
        let _ = fs::remove_file(marker);
    } else if fs::write(marker, "disabled").is_err() {
        return;
    }
    write_autostart_entry(enabled);
}

fn write_autostart_entry(enabled: bool) {
    let path = autostart_path();
    if !enabled {
        let _ = fs::remove_file(path);
        return;
    }
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let (exec, try_exec) = if std::path::Path::new("/.flatpak-info").is_file() {
        (
            "flatpak run --command=packlift io.github.luis_gustavo_git_hub.Packlift --background-updates",
            "flatpak",
        )
    } else {
        ("packlift --background-updates", "packlift")
    };
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Packlift background updates\nComment=Check and install updates in the background\nExec={exec}\nTryExec={try_exec}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
    );
    let temporary = path.with_extension("desktop.tmp");
    if fs::write(&temporary, entry).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

pub(super) fn ensure_background_autostart() {
    let enabled = start_with_system_enabled();
    write_autostart_entry(enabled);
}

fn start_background_system_check(app: &gtk::Application) {
    if !automatic_updates_can_use_network()
        || running_on_battery()
        || operations_ui::has_active_title(tr("Updating system"))
        || BACKGROUND_SYSTEM_CHECK_RUNNING.with(|running| running.replace(true))
    {
        return;
    }
    let app = app.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let _ = gio::spawn_blocking(|| check(Source::System)).await;
        record_last_check(Source::System, now_epoch());
        BACKGROUND_SYSTEM_CHECK_RUNNING.with(|running| running.set(false));
        if let Some(app) = app.upgrade() {
            schedule_due_updates(&app);
        }
    });
}

pub(super) fn run_background(app: &adw::Application) {
    if BACKGROUND_MODE_STARTED.with(|started| started.replace(true)) {
        return;
    }
    // Keep the headless GTK application alive for its scheduled checks.
    std::mem::forget(app.hold());
    crate::tray::start();
    migrate_schedule();
    let app = app.upcast_ref::<gtk::Application>().downgrade();
    if let Some(app) = app.upgrade() {
        start_background_system_check(&app);
    }
    let schedule_app = app.clone();
    glib::timeout_add_seconds_local(60, move || {
        let Some(app) = schedule_app.upgrade() else {
            return glib::ControlFlow::Break;
        };
        schedule_due_updates(&app);
        glib::ControlFlow::Continue
    });
    let check_app = app;
    glib::timeout_add_seconds_local(30 * 60, move || {
        let Some(app) = check_app.upgrade() else {
            return glib::ControlFlow::Break;
        };
        start_background_system_check(&app);
        glib::ControlFlow::Continue
    });
}

impl Source {
    fn schedule_keys(self) -> (&'static str, &'static str) {
        match self {
            Self::System => ("system_interval_days", "system_last_auto_update"),
            Self::Flatpaks => ("flatpak_interval_days", "flatpak_last_auto_update"),
            Self::AppImages => ("interval_days", "last_auto_update"),
        }
    }

    fn auto_title(self) -> &'static str {
        match self {
            Self::System => tr("Automatic system updates"),
            Self::Flatpaks => tr("Automatic Flatpak updates"),
            Self::AppImages => tr("Automatic AppImage updates"),
        }
    }
}

fn read_schedule(source: Source) -> (u64, u64) {
    let state = update_state();
    let (interval, last) = source.schedule_keys();
    let days = state[interval]
        .as_u64()
        .or_else(|| {
            (source != Source::System)
                .then(|| state["interval_days"].as_u64())
                .flatten()
        })
        .filter(|days| UPDATE_INTERVAL_DAYS.contains(days))
        .unwrap_or(1);
    let last_run = state[last]
        .as_u64()
        .or_else(|| {
            (source != Source::System)
                .then(|| state["last_auto_update"].as_u64())
                .flatten()
        })
        .unwrap_or(0);
    (days, last_run)
}

// Freeze the previous shared schedule before either preference is changed.
fn migrate_schedule() {
    let mut state = update_state();
    let (interval, last) = Source::Flatpaks.schedule_keys();
    if state[interval].is_null() || state[last].is_null() {
        let (days, last_run) = read_schedule(Source::Flatpaks);
        state[interval] = serde_json::json!(days);
        state[last] = serde_json::json!(last_run);
        save_update_state(state);
    }
}

fn update_state() -> serde_json::Value {
    fs::read_to_string(schedule_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn save_update_state(value: serde_json::Value) {
    let path = schedule_path();
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let temporary = path.with_extension("json.tmp");
    if fs::write(&temporary, value.to_string()).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

fn write_schedule(source: Source, days: u64, last_run: u64) {
    let mut state = update_state();
    let (interval, last) = source.schedule_keys();
    state[interval] = serde_json::json!(days);
    state[last] = serde_json::json!(last_run);
    save_update_state(state);
}

fn check_key(source: Source) -> &'static str {
    match source {
        Source::System => "last_checked_system",
        Source::Flatpaks => "last_checked_flatpaks",
        Source::AppImages => "last_checked_appimages",
    }
}

fn last_check_time(source: Source) -> String {
    let timestamp = update_state()[check_key(source)].as_i64();
    let formatted = timestamp
        .and_then(|timestamp| glib::DateTime::from_unix_local(timestamp).ok())
        .and_then(|date| date.format("%x %H:%M").ok())
        .map(|date| date.to_string());
    formatted.map_or_else(
        || tr("Not checked yet").to_owned(),
        |time| tr("Last checked: {time}").replace("{time}", &time),
    )
}

fn record_last_check(source: Source, timestamp: u64) -> String {
    let mut state = update_state();
    state[check_key(source)] = serde_json::json!(timestamp);
    save_update_state(state);
    last_check_time(source)
}

fn format_flatpak_status(result: &Available) -> String {
    let apps = result.packages.len();
    match (apps, result.runtime_updates) {
        (1, 0) => tr("1 app needs updates").to_owned(),
        (count, 0) => tr("{count} apps need updates").replace("{count}", &count.to_string()),
        (0, 1) => tr("1 runtime update available").to_owned(),
        (0, runtimes) => {
            tr("{count} runtime updates available").replace("{count}", &runtimes.to_string())
        }
        (count, runtimes) => tr("{count} apps and {runtimes} runtime updates available")
            .replace("{count}", &count.to_string())
            .replace("{runtimes}", &runtimes.to_string()),
    }
}

fn system_updater(runner: &Runner) -> Option<SystemUpdater> {
    if runner.available("pkcon") {
        Some(SystemUpdater::PackageKit)
    } else if runner.available("rpm-ostree") {
        Some(SystemUpdater::RpmOstree)
    } else {
        None
    }
}

fn check_system(runner: &Runner) -> Result<Available> {
    let Some(updater) = system_updater(runner) else {
        return Err(PackliftError::Message(
            tr("System update support is unavailable for this distribution.").into(),
        ));
    };
    runner.write(tr("Checking for system updates"));
    let output = match updater {
        SystemUpdater::RpmOstree => runner.capture(
            &system_command(&[
                "sh",
                "-c",
                "rpm-ostree upgrade --check; result=$?; if test \"$result\" -eq 77; then exit 0; fi; exit \"$result\"",
            ]),
            false,
        )?,
        SystemUpdater::PackageKit => {
            runner.run(
                &system_command(&["pkcon", "--noninteractive", "refresh", "force"]),
                false,
                true,
                true,
            )?;
            runner.capture(
                &system_command(&["pkcon", "--noninteractive", "get-updates"]),
                false,
            )?
        }
    };
    let available = match updater {
        SystemUpdater::RpmOstree => {
            let output = output.to_ascii_lowercase();
            if output.contains("no upgrade available") || output.contains("no updates available") {
                false
            } else if output.contains("automatic updates are not enabled") {
                return Err(PackliftError::Message(
                    tr("rpm-ostree cannot check updates until an update policy is enabled.").into(),
                ));
            } else if !output.trim().is_empty() {
                true
            } else {
                return Err(PackliftError::Message(
                    tr("Could not read the system update check result. See the Packlift log.")
                        .into(),
                ));
            }
        }
        SystemUpdater::PackageKit => {
            let output = output.to_ascii_lowercase();
            !output.contains("no updates available")
                && output.lines().map(str::trim).any(|line| {
                    !line.is_empty()
                        && !line.starts_with("getting updates")
                        && !line.starts_with("finished")
                        && !line.starts_with("refreshing")
                        && !line.starts_with("loading")
                })
        }
    };
    Ok(Available {
        backend: Backend::System(updater),
        available,
        packages: if available {
            vec![tr("System update").to_owned()]
        } else {
            Vec::new()
        },
        runtime_updates: 0,
        failed_remotes: Vec::new(),
        unsupported: 0,
        supported: 1,
    })
}

fn register_update_sections(sections: &[Rc<Section>]) {
    UPDATE_SECTIONS.with_borrow_mut(|registered| {
        registered.retain(|section| section.strong_count() > 0);
        registered.extend(sections.iter().map(Rc::downgrade));
    });
}

fn ensure_auto_check_timer() {
    if AUTO_CHECK_TIMER_STARTED.with(|started| started.replace(true)) {
        return;
    }
    glib::timeout_add_seconds_local(30 * 60, || {
        refresh_visible_sections();
        glib::ControlFlow::Continue
    });
}

fn refresh_visible_sections() {
    let sections = UPDATE_SECTIONS.with_borrow_mut(|registered| {
        let sections = registered
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect::<Vec<_>>();
        registered.retain(|section| section.strong_count() > 0);
        sections
    });
    for section in sections {
        if section.status.root().is_some() {
            section.refresh();
        }
    }
}

fn distribution_identity() -> (String, String) {
    let result = Runner::new(None, Arc::new(|_| {}))
        .and_then(|runner| runner.capture(&command(&["cat", "/etc/os-release"]), false));
    let Ok(contents) = result else {
        return (
            tr("Linux distribution").to_owned(),
            "computer-symbolic".to_owned(),
        );
    };
    let values: BTreeMap<_, _> = contents
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key, value.trim_matches('"')))
        .collect();
    let name = values
        .get("PRETTY_NAME")
        .copied()
        .unwrap_or("Linux distribution")
        .to_owned();
    let candidate = values
        .get("LOGO")
        .map(|logo| (*logo).to_owned())
        .or_else(|| values.get("ID").map(|id| format!("distributor-logo-{id}")))
        .unwrap_or_else(|| "computer-symbolic".to_owned());
    (name, candidate)
}

fn apply_due_update(source: Source, log: LogHandler) -> Result<()> {
    match source {
        Source::System => {
            let runner = Runner::new(None, log.clone())?;
            let available = check_system(&runner)?;
            if available.available {
                apply_with_mode(&available.backend, log, true)?;
            }
        }
        Source::Flatpaks => {
            let _operation = operation(&packlift_data_dir())?;
            let runner = Runner::new(None, log)?;
            runner.write("Checking Flatpak updates");
            let mut errors = Vec::new();
            for (scope, stage) in [
                ("--user", "Updating Flatpaks for my account"),
                ("--system", "Updating system Flatpaks"),
            ] {
                runner.write(stage);
                if let Err(error) = runner.run(
                    &command(&[
                        "flatpak",
                        "update",
                        scope,
                        "--noninteractive",
                        "--assumeyes",
                    ]),
                    false,
                    true,
                    true,
                ) {
                    errors.push(error.to_string());
                }
            }
            if !errors.is_empty() {
                return Err(PackliftError::Message(errors.join("\n")));
            }
        }
        Source::AppImages => {
            let runner = Runner::new(None, log.clone())?;
            if appimage_updater(&runner).is_none() {
                return Ok(());
            }
            let available = check_appimages(&runner)?;
            if available.available {
                apply(&available.backend, log)?;
            }
        }
    }
    Ok(())
}

fn schedule_due_updates(app: &gtk::Application) {
    let now = now_epoch();
    if !automatic_updates_can_use_network() || running_on_battery() {
        return;
    }
    for source in [Source::System, Source::Flatpaks, Source::AppImages] {
        if source == Source::System && BACKGROUND_SYSTEM_CHECK_RUNNING.with(Cell::get) {
            continue;
        }
        if matches!(source, Source::System | Source::Flatpaks) && !host_access_available() {
            continue;
        }
        let (days, last_run) = read_schedule(source);
        let schedule_jitter = match source {
            Source::System => 131,
            Source::Flatpaks => 173,
            Source::AppImages => 419,
        } + last_run % 127;
        if days == 0
            || (last_run != 0
                && now.saturating_sub(last_run) < days * 24 * 60 * 60 + schedule_jitter)
        {
            continue;
        }
        let title = source.auto_title();
        if operations_ui::has_active_title(title)
            || (source == Source::AppImages
                && operations_ui::has_active_title(tr("Updating AppImages")))
        {
            continue;
        }
        let retry_later = AUTO_RETRY.with_borrow(|attempts| {
            attempts
                .get(&source)
                .is_some_and(|last| last.elapsed() < Duration::from_secs(60 * 60))
        });
        if retry_later {
            continue;
        }
        AUTO_RETRY.with_borrow_mut(|attempts| attempts.insert(source, Instant::now()));
        operations_ui::submit(
            app,
            title,
            move |log| apply_due_update(source, log).map(|_| None),
            move |result| {
                if result.is_ok() {
                    write_schedule(source, read_schedule(source).0, now_epoch());
                }
                invalidate_check(source);
                refresh_visible_sections();
            },
            || {},
        );
    }
}

fn ensure_auto_update_timer(app: &gtk::Application) {
    if AUTO_UPDATE_TIMER_STARTED.with(|started| started.replace(true)) {
        return;
    }
    migrate_schedule();
    let start = app.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(app) = start.upgrade() {
            schedule_due_updates(&app);
        }
    });
    let app = app.downgrade();
    glib::timeout_add_seconds_local(60, move || {
        let Some(app) = app.upgrade() else {
            return glib::ControlFlow::Break;
        };
        schedule_due_updates(&app);
        glib::ControlFlow::Continue
    });
}

fn check(source: Source) -> Result<Available> {
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    match source {
        Source::System => check_system(&runner),
        Source::Flatpaks => check_all_flatpaks(&runner),
        Source::AppImages => check_appimages(&runner),
    }
}

fn appimage_updater(runner: &Runner) -> Option<(&'static str, bool)> {
    if runner.sandbox {
        return std::path::Path::new("/app/bin/appimageupdate")
            .is_file()
            .then_some(("/app/bin/appimageupdate", true));
    }
    if runner.available("appimageupdate") {
        Some(("appimageupdate", false))
    } else if runner.available("appimageupdatetool") {
        Some(("appimageupdatetool", false))
    } else {
        None
    }
}

fn check_appimages(runner: &Runner) -> Result<Available> {
    use packlift::system::Registry;
    let Some((updater, local)) = appimage_updater(runner) else {
        return Err(PackliftError::Message(
            tr("AppImage update support is unavailable.").into(),
        ));
    };
    let data = packlift_data_dir();
    let registry = Registry::new(&data)?;
    let mut updates = Vec::new();
    let mut names = Vec::new();
    let mut failed = Vec::new();
    let mut unsupported = 0;
    let mut supported = 0;

    for app in registry.apps()? {
        if app["method"] != "local" {
            continue;
        }
        let Some(id) = app["id"].as_str() else {
            continue;
        };
        let Some(path) = managed_appimage_path(&data, id) else {
            continue;
        };
        if !path.is_file() {
            let sandbox_only = app["sandbox_only"].as_bool() == Some(true);
            let migrated = sandbox_only
                && app["source"]
                    .as_str()
                    .map(std::path::Path::new)
                    .filter(|source| {
                        source.is_file()
                            && source
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("appimage"))
                    })
                    .is_some_and(|source| fs::copy(source, &path).is_ok());
            if !migrated {
                unsupported += if sandbox_only { 1 } else { 0 };
                continue;
            }
        }
        let name = app["name"].as_str().unwrap_or("AppImage");
        let source = match normalize_appimage_source(
            app["appimage_update_source"].as_str().unwrap_or_default(),
        ) {
            Ok(source) => source,
            Err(error) => {
                failed.push(format!("{name}: {error}"));
                continue;
            }
        };
        if source.is_some() && updater == "appimageupdatetool" {
            failed.push(format!(
                "{name}: {}",
                tr("This updater cannot use a custom AppImage source.")
            ));
            continue;
        }
        let target = AppImageUpdate {
            id: id.to_owned(),
            path,
            source,
        };
        if target.source.is_none() {
            let describe = runner.capture(&target.command(updater, "--describe"), local);
            if !describe.is_ok_and(|output| {
                output.lines().any(|line| {
                    line.trim_start()
                        .strip_prefix("Update info:")
                        .is_some_and(|info| !info.trim().is_empty())
                })
            }) {
                unsupported += 1;
                continue;
            }
        }
        supported += 1;
        let result = match runner.run(
            &target.command(updater, "--check-for-update"),
            local,
            true,
            false,
        ) {
            Ok(result) => result,
            Err(error) => {
                failed.push(format!("{name}: {error}"));
                continue;
            }
        };
        let diagnostic = String::from_utf8_lossy(&result.stderr);
        if diagnostic.contains("Error:") {
            failed.push(format!(
                "{}: {}",
                app["name"].as_str().unwrap_or("AppImage"),
                diagnostic.trim()
            ));
            continue;
        }
        match result.status.code() {
            Some(0) => {}
            Some(1) => {
                updates.push(target);
                names.push(app["name"].as_str().unwrap_or("AppImage").to_owned());
            }
            _ => {
                let detail = diagnostic.lines().last().unwrap_or("").trim().to_owned();
                failed.push(if detail.is_empty() {
                    app["name"].as_str().unwrap_or("AppImage").to_owned()
                } else {
                    format!("{}: {detail}", app["name"].as_str().unwrap_or("AppImage"))
                });
            }
        }
    }

    Ok(Available {
        backend: Backend::AppImages(updater.to_owned(), updates),
        available: !names.is_empty(),
        packages: names,
        runtime_updates: 0,
        failed_remotes: failed,
        unsupported,
        supported,
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
    let mut app_names = BTreeSet::new();
    let mut runtime_updates = 0;
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
                for reference in output
                    .split_whitespace()
                    .filter(|reference| installed.contains(flatpak_ref(reference)))
                {
                    updates.insert(reference.to_owned());
                    if let Some(app_ref) = reference.strip_prefix("app/") {
                        if let Some(name) = app_ref.split('/').next() {
                            app_names.insert(name.to_owned());
                        }
                    } else if reference.starts_with("runtime/") {
                        runtime_updates += 1;
                    }
                }
            }
            Err(_) => failed_remotes.push(origin.to_owned()),
        }
    }
    Ok(Available {
        available: !updates.is_empty(),
        backend: Backend::Flatpak(scope, updates.into_iter().collect()),
        packages: app_names.into_iter().collect(),
        runtime_updates,
        failed_remotes,
        unsupported: 0,
        supported: 0,
    })
}

fn check_all_flatpaks(runner: &Runner) -> Result<Available> {
    let mut scopes = Vec::new();
    let mut packages = BTreeSet::new();
    let mut runtime_updates = 0;
    let mut failed_remotes = Vec::new();
    let mut errors = Vec::new();
    for scope in ["--user", "--system"] {
        match check_flatpaks(runner, scope) {
            Ok(result) => {
                packages.extend(result.packages);
                runtime_updates += result.runtime_updates;
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
        return Err(PackliftError::Message(failed_remotes.join("\n")));
    }
    let available = scopes.iter().any(|(_, refs)| !refs.is_empty());
    Ok(Available {
        backend: Backend::Flatpaks(scopes),
        available,
        packages: packages.into_iter().collect(),
        runtime_updates,
        failed_remotes,
        unsupported: 0,
        supported: 0,
    })
}

fn flatpak_ref(reference: &str) -> &str {
    reference
        .strip_prefix("app/")
        .or_else(|| reference.strip_prefix("runtime/"))
        .unwrap_or(reference)
}

fn apply(backend: &Backend, log: LogHandler) -> Result<()> {
    apply_with_mode(backend, log, false)
}

fn apply_with_mode(backend: &Backend, log: LogHandler, noninteractive: bool) -> Result<()> {
    let _operation = operation(&packlift_data_dir())?;
    let runner = Runner::new(None, log)?;
    match backend {
        Backend::System(SystemUpdater::RpmOstree) => {
            runner.write(tr("Staging the system update"));
            runner.run(
                &system_command(&["rpm-ostree", "upgrade"]),
                false,
                true,
                true,
            )?;
        }
        Backend::System(SystemUpdater::PackageKit) => {
            runner.write(tr("Installing system updates"));
            let args = if noninteractive {
                system_command(&["pkcon", "--noninteractive", "update-system"])
            } else {
                system_command(&["pkcon", "update-system"])
            };
            runner.run(&args, false, true, true)?;
        }
        Backend::Flatpak(scope, refs) => {
            if !refs.is_empty() {
                runner.run(
                    &command(&["flatpak", "update", scope, "--assumeyes"]),
                    false,
                    true,
                    true,
                )?;
            }
        }
        Backend::Flatpaks(scopes) => {
            for (scope, _) in scopes.iter().filter(|(_, refs)| !refs.is_empty()) {
                runner.run(
                    &command(&["flatpak", "update", scope, "--assumeyes"]),
                    false,
                    true,
                    true,
                )?;
            }
        }
        Backend::AppImages(updater, paths) => {
            let records = packlift::system::Registry::new(&runner.data)?.apps()?;
            for target in paths {
                let current = records
                    .iter()
                    .find(|app| app["id"].as_str() == Some(&target.id) && app["method"] == "local");
                let Some(app) = current else {
                    continue;
                };
                let source = normalize_appimage_source(
                    app["appimage_update_source"].as_str().unwrap_or_default(),
                )?;
                if source != target.source || !target.path.is_file() {
                    continue;
                }
                let path = &target.path;
                runner.write("Applying AppImage update");
                let result = runner.run(
                    &target.command(updater, "--overwrite"),
                    runner.sandbox,
                    true,
                    false,
                )?;
                if !result.status.success() {
                    return Err(PackliftError::Message(format!(
                        "{} could not be updated. See the Packlift log.",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("AppImage")
                    )));
                }
                if let Err(error) = refresh_appimage_payload(&runner, path) {
                    restore_appimage_backup(path);
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn refresh_appimage_payload(runner: &Runner, image: &std::path::Path) -> Result<()> {
    use std::fs;

    let directory = image.parent().ok_or_else(|| {
        PackliftError::Message("The AppImage installation directory is missing.".into())
    })?;
    let payload = directory.join("payload");
    let replacement = directory.join("payload.update");
    let backup = directory.join("payload.old");
    if replacement.exists() {
        fs::remove_dir_all(&replacement)?;
    }
    let offset = packlift::installer::appimage_offset(image)?.ok_or_else(|| {
        PackliftError::Message("The updated file is not a supported type 2 AppImage.".into())
    })?;
    let offset_text = offset.to_string();
    let replacement_text = replacement.to_string_lossy().into_owned();
    let image_text = image.to_string_lossy().into_owned();
    runner.run(
        &command(&[
            "unsquashfs",
            "-no-progress",
            "-o",
            &offset_text,
            "-d",
            &replacement_text,
            &image_text,
        ]),
        true,
        true,
        true,
    )?;
    let canonical_replacement = fs::canonicalize(&replacement)?;
    let app_run = fs::canonicalize(replacement.join("AppRun")).map_err(|_| {
        PackliftError::Message("The updated AppImage has no usable AppRun entry.".into())
    })?;
    if !app_run.starts_with(&canonical_replacement) || !app_run.is_file() {
        fs::remove_dir_all(&replacement)?;
        return Err(PackliftError::Message(
            "The updated AppImage AppRun points outside its payload or is not a file.".into(),
        ));
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    fs::rename(&payload, &backup)?;
    if let Err(error) = fs::rename(&replacement, &payload) {
        let _ = fs::rename(&backup, &payload);
        return Err(error.into());
    }
    Ok(())
}

fn restore_appimage_backup(image: &std::path::Path) {
    let backup = image.with_file_name("program.AppImage.old");
    let failed = image.with_file_name("program.AppImage.failed");
    if backup.is_file() {
        let _ = fs::remove_file(&failed);
        if fs::rename(image, &failed).is_ok() {
            let _ = fs::rename(backup, image);
        }
    }
}

struct Section {
    source: Source,
    status: gtk::Label,
    last_checked: gtk::Label,
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
        if source == Source::System {
            group.add_css_class("packlift-update-system");
        } else {
            group.add_css_class("packlift-update-section");
        }
        if source == Source::Flatpaks {
            group.add_css_class("packlift-update-flatpak");
        }
        if source != Source::System {
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let icon = gtk::Image::from_icon_name(icon_name);
            icon.set_pixel_size(if source == Source::Flatpaks { 40 } else { 18 });
            if source == Source::Flatpaks {
                icon.add_css_class("packlift-flatpak-update-icon");
            }
            heading.append(&icon);
            let heading_title = home_label(title, "heading");
            if source == Source::Flatpaks {
                heading_title.set_wrap(false);
                heading_title.set_single_line_mode(true);
            }
            heading.append(&heading_title);
            group.append(&heading);
        }
        let status = home_label(tr("Waiting to check for updates."), "dim-label");
        group.append(&status);
        let last_checked = home_label(&last_check_time(source), "dim-label");
        last_checked.add_css_class("caption");
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
        group.append(&last_checked);
        group.append(&update);
        group.append(&automatic_preferences(source));
        panel.append(&group);
        let section = Rc::new(Self {
            source,
            status,
            last_checked,
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
        self.status.set_visible(true);
        self.last_checked.set_visible(true);
        self.status.set_text(tr("Checking for updates…"));
        let source = self.source;
        let (cached, should_check) = CHECKS.with_borrow_mut(|checks| {
            let state = checks.entry(source).or_default();
            if let Some((time, result, checked_at)) = &state.cached {
                if time.elapsed() < Duration::from_secs(29 * 60) {
                    return (Some((result.clone(), checked_at.clone())), false);
                }
            }
            let should_check = state.waiting.is_empty();
            state.waiting.push(Rc::downgrade(self));
            (None, should_check)
        });
        if let Some((result, checked_at)) = cached {
            self.show_result(&result, &checked_at);
        }
        if should_check {
            glib::MainContext::default().spawn_local(async move {
                let result = gio::spawn_blocking(move || check(source))
                    .await
                    .map_err(|_| {
                        tr("Could not check for updates. See the Packlift log.").to_owned()
                    })
                    .and_then(|result| result.map_err(|error| error.to_string()));
                let (waiting, invalidated) = CHECKS.with_borrow_mut(|checks| {
                    let state = checks.entry(source).or_default();
                    (
                        std::mem::take(&mut state.waiting),
                        std::mem::take(&mut state.invalidated),
                    )
                });
                let waiting = waiting.into_iter().filter_map(|section| section.upgrade());
                if invalidated {
                    for section in waiting {
                        section.busy.set(false);
                        section.refresh();
                    }
                    return;
                }
                let checked_at = record_last_check(source, now_epoch());
                let result = Rc::new(result);
                CHECKS.with_borrow_mut(|checks| {
                    checks.entry(source).or_default().cached =
                        Some((Instant::now(), result.clone(), checked_at.clone()));
                });
                for section in waiting {
                    section.show_result(&result, &checked_at);
                }
            });
        }
    }

    fn show_result(&self, result: &CheckResult, checked_at: &str) {
        self.busy.set(false);
        self.last_checked.set_text(checked_at);
        match result {
            Ok(result) => {
                *self.backend.borrow_mut() = result.available.then_some(result.backend.clone());
                let partial = !result.failed_remotes.is_empty();
                let unsupported =
                    matches!(self.source, Source::AppImages) && result.unsupported > 0;
                self.warning.set_visible(partial || unsupported);
                let mut warnings = Vec::new();
                if partial {
                    warnings.push(if matches!(self.source, Source::Flatpaks) {
                        tr("Some Flatpak repositories could not be checked.").to_owned()
                    } else {
                        tr("Some AppImages could not be checked.").to_owned()
                    });
                }
                if unsupported {
                    warnings.push(
                        tr("{count} AppImages do not support automatic updates.")
                            .replace("{count}", &result.unsupported.to_string()),
                    );
                }
                self.warning.set_text(&warnings.join("\n"));
                let status = if result.available && matches!(self.source, Source::Flatpaks) {
                    format_flatpak_status(result)
                } else if result.available {
                    match self.source {
                        Source::System => tr("System update available").to_owned(),
                        Source::AppImages => {
                            let count = result.packages.len();
                            if count == 1 {
                                tr("1 AppImage update available").to_owned()
                            } else {
                                tr("{count} AppImage updates available")
                                    .replace("{count}", &count.to_string())
                            }
                        }
                        Source::Flatpaks => tr("Updates available").to_owned(),
                    }
                } else if self.source == Source::System {
                    String::new()
                } else if partial {
                    tr("Update check incomplete").to_owned()
                } else if matches!(self.source, Source::AppImages) && result.supported > 0 {
                    tr("All supported AppImages are up to date.").to_owned()
                } else if matches!(self.source, Source::AppImages) {
                    tr("No AppImages with automatic update support were found.").to_owned()
                } else {
                    String::new()
                };
                self.status.set_text(&status);
                self.status
                    .set_visible(result.available || partial || self.source == Source::AppImages);
                let mut lines = result.packages.iter().take(8).cloned().collect::<Vec<_>>();
                if result.packages.len() > 8 {
                    lines.push(
                        tr("And {count} more")
                            .replace("{count}", &(result.packages.len() - 8).to_string()),
                    );
                }
                self.details.set_text(&lines.join("\n"));
                self.details
                    .set_visible(!lines.is_empty() && matches!(self.source, Source::AppImages));
                self.update.set_visible(
                    result.available && matches!(self.source, Source::System | Source::AppImages),
                );
                self.last_checked
                    .set_visible(!(self.source == Source::System && result.available));
            }
            Err(error) => {
                *self.backend.borrow_mut() = None;
                self.update.set_visible(false);
                self.last_checked.set_visible(true);
                self.status.set_text(error);
            }
        }
    }

    fn start_update(self: &Rc<Self>, app: &gtk::Application) {
        let title = match self.source {
            Source::System => tr("Updating system"),
            Source::Flatpaks => tr("Updating Flatpaks"),
            Source::AppImages => tr("Updating AppImages"),
        };
        if operations_ui::has_active_title(title)
            || operations_ui::has_active_title(self.source.auto_title())
        {
            return;
        }
        let Some(backend) = self.backend.borrow().clone() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.status.set_text(tr("Applying updates…"));
        self.update.set_sensitive(false);
        let completed = self.clone();
        let released = self.clone();
        operations_ui::submit(
            app,
            title,
            move |log| apply(&backend, log).map(|_| None),
            move |result| {
                if let Err(error) = result {
                    completed.status.set_text(&error.to_string());
                } else {
                    *completed.backend.borrow_mut() = None;
                    write_schedule(
                        completed.source,
                        read_schedule(completed.source).0,
                        now_epoch(),
                    );
                    invalidate_check(completed.source);
                }
            },
            move || {
                released.busy.set(false);
                released.update.set_sensitive(true);
                if released.backend.borrow().is_none() {
                    released.refresh();
                    refresh_visible_sections();
                } else if released.status.text() == tr("Applying updates…") {
                    released.refresh();
                }
            },
        );
    }
}

fn automatic_preferences(source: Source) -> gtk::Box {
    let preferences = gtk::Box::new(gtk::Orientation::Vertical, 8);
    preferences.add_css_class("packlift-update-preferences");
    let automatic_title = tr("Install updates automatically");
    let helper = match source {
        Source::System => {
            tr("Checks and installs system updates while Packlift is open. Some updates may require a restart.")
        }
        Source::Flatpaks => tr("Updates apps and runtimes while Packlift is open."),
        Source::AppImages => tr("Updates supported AppImages while Packlift is open."),
    };
    let row = adw::SwitchRow::builder()
        .title(automatic_title)
        .subtitle(helper)
        .build();
    let frequency = gtk::DropDown::from_strings(&[tr("Daily"), tr("Weekly"), tr("Monthly")]);
    frequency.set_valign(gtk::Align::Center);
    frequency.update_property(&[gtk::accessible::Property::Label(tr(
        "Automatic update frequency",
    ))]);
    let frequency_row = adw::ActionRow::builder().title(tr("Frequency")).build();
    frequency_row.add_suffix(&frequency);
    frequency_row.set_activatable_widget(Some(&frequency));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list.append(&row);
    list.append(&frequency_row);
    if source == Source::System {
        let startup = adw::SwitchRow::builder()
            .title(tr("Start Packlift with the system"))
            .subtitle(tr(
                "Check for updates in the background when you sign in. A tray icon shows when supported.",
            ))
            .build();
        startup.set_active(start_with_system_enabled());
        list.append(&startup);
        startup.connect_active_notify(|row| set_start_with_system(row.is_active()));
    }
    preferences.append(&list);
    if !host_access_available() {
        match source {
            Source::System => {
                row.set_sensitive(false);
                frequency_row.set_sensitive(false);
            }
            Source::Flatpaks => preferences.set_sensitive(false),
            Source::AppImages => {}
        }
    }

    let loading = Rc::new(Cell::new(false));
    let sync = {
        let (row, frequency) = (row.downgrade(), frequency.downgrade());
        let loading = loading.clone();
        move || {
            let (Some(row), Some(frequency)) = (row.upgrade(), frequency.upgrade()) else {
                return;
            };
            let (days, _) = read_schedule(source);
            loading.set(true);
            if days > 0 {
                frequency.set_selected(match days {
                    7 => 1,
                    30 => 2,
                    _ => 0,
                });
            }
            row.set_active(days > 0);
            frequency.set_sensitive(days > 0);
            loading.set(false);
        }
    };
    sync();
    preferences.connect_map(move |_| sync());

    let set_schedule = Rc::new({
        let (row, frequency) = (row.downgrade(), frequency.downgrade());
        move || {
            if loading.get() {
                return;
            }
            let (Some(row), Some(frequency)) = (row.upgrade(), frequency.upgrade()) else {
                return;
            };
            let days = if row.is_active() {
                [1, 7, 30]
                    .get(frequency.selected() as usize)
                    .copied()
                    .unwrap_or(1)
            } else {
                0
            };
            frequency.set_sensitive(days > 0);
            migrate_schedule();
            let (_, last_run) = read_schedule(source);
            write_schedule(source, days, last_run);
            if days == 0 {
                operations_ui::cancel_queued(source.auto_title());
            } else {
                AUTO_RETRY.with_borrow_mut(|attempts| attempts.remove(&source));
            }
        }
    });
    let toggle = set_schedule.clone();
    row.connect_active_notify(move |_| toggle());
    frequency.connect_selected_notify(move |_| set_schedule());
    preferences
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
    split.set_pin_sidebar(true);
    split.set_enable_show_gesture(false);
    split.set_collapsed(true);
    split.set_show_sidebar(false);
    if window.widget_name() == "packlift-library" {
        let responsive = adw::BreakpointBin::builder()
            .width_request(360)
            .height_request(300)
            .child(&split)
            .build();
        let wide = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MinWidth,
            1200.0,
            adw::LengthUnit::Px,
        ));
        wide.add_setters(&[(&split, "collapsed", false)]);
        responsive.add_breakpoint(wide);
        toolbar.set_content(Some(&responsive));
    } else {
        toolbar.set_content(Some(&split));
    }

    let pages = gtk::Stack::new();
    let updates = gtk::ToggleButton::new();
    updates.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("software-update-available-symbolic")
            .label(tr("Updates"))
            .build(),
    ));
    let operations_label = adw::ButtonContent::builder()
        .icon_name("view-list-symbolic")
        .label(tr("Operations"))
        .build();
    let operations = gtk::ToggleButton::new();
    operations.set_child(Some(&operations_label));
    header.pack_end(&updates);
    header.pack_end(&operations);
    let weak_label = operations_label.downgrade();
    operations_ui::observe(&operations, move || {
        if let Some(label) = weak_label.upgrade() {
            let count = operations_ui::active_count();
            label.set_label(&if count == 0 {
                tr("Operations").to_owned()
            } else {
                format!("{} ({count})", tr("Operations"))
            });
        }
    });
    if operations_ui::active_count() > 0 {
        operations_label.set_label(&format!(
            "{} ({})",
            tr("Operations"),
            operations_ui::active_count()
        ));
    }

    for (button, name) in [(&updates, "updates"), (&operations, "operations")] {
        let panel = split.downgrade();
        let stack = pages.downgrade();
        button.connect_clicked(move |_| {
            let (Some(panel), Some(stack)) = (panel.upgrade(), stack.upgrade()) else {
                return;
            };
            let was_open =
                panel.shows_sidebar() && stack.visible_child_name().as_deref() == Some(name);
            stack.set_visible_child_name(name);
            panel.set_show_sidebar(!was_open);
            // Keep the toggles in sync when switching from one panel to the other.
            panel.notify("show-sidebar");
        });
    }
    let (update_button, operation_button, stack) = (
        updates.downgrade(),
        operations.downgrade(),
        pages.downgrade(),
    );
    split.connect_show_sidebar_notify(move |panel| {
        let (Some(updates), Some(operations), Some(stack)) = (
            update_button.upgrade(),
            operation_button.upgrade(),
            stack.upgrade(),
        ) else {
            return;
        };
        let selected = stack.visible_child_name();
        updates.set_active(panel.shows_sidebar() && selected.as_deref() == Some("updates"));
        operations.set_active(panel.shows_sidebar() && selected.as_deref() == Some("operations"));
    });

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
    let distro_name = home_label(tr("Linux distribution"), "title-2");
    distro_name.set_hexpand(true);
    distro_name.set_valign(gtk::Align::Center);
    distribution.append(&distro_name);
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
    sidebar.append(&distro_module);

    let system = Section::new(Source::System, "", "", &distro_module, window);
    let flatpaks = Section::new(
        Source::Flatpaks,
        tr("Flatpak"),
        "packlift-flatpak",
        &sidebar,
        window,
    );
    let appimages = Section::new(
        Source::AppImages,
        tr("AppImages"),
        "application-x-executable-symbolic",
        &sidebar,
        window,
    );
    let sections = [system, flatpaks, appimages];
    register_update_sections(&sections);
    ensure_auto_check_timer();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&sidebar)
        .build();
    pages.add_named(&scroll, Some("updates"));
    let activity = gtk::Box::new(gtk::Orientation::Vertical, 12);
    activity.set_margin_top(16);
    activity.set_margin_bottom(16);
    activity.set_margin_start(16);
    activity.set_margin_end(16);
    activity.append(&home_label(tr("Operations"), "title-2"));
    activity.append(&operations_ui::panel());
    let activity_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&activity)
        .build();
    pages.add_named(&activity_scroll, Some("operations"));
    split.set_sidebar(Some(&pages));

    let refresh_all = move || {
        for section in &sections {
            section.refresh();
        }
    };
    if let Some(app) = window.application() {
        ensure_auto_update_timer(&app);
    }
    let checked = Cell::new(false);
    toolbar.connect_map(move |_| {
        if !checked.replace(true) {
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

#[cfg(test)]
mod appimage_tests {
    use super::*;

    #[test]
    fn appimage_check_and_apply_use_the_same_custom_source() {
        let mut target = AppImageUpdate {
            id: "0123456789abcdef0123".into(),
            path: "/tmp/file with spaces.AppImage".into(),
            source: Some("zsync|https://example.com/app.AppImage.zsync".into()),
        };
        for mode in ["--check-for-update", "--overwrite"] {
            assert_eq!(
                target.command("appimageupdate", mode),
                [
                    "appimageupdate",
                    mode,
                    "--update-info=zsync|https://example.com/app.AppImage.zsync",
                    "--",
                    "/tmp/file with spaces.AppImage",
                ]
            );
        }
        target.source = None;
        assert_eq!(
            target.command("appimageupdate", "--check-for-update"),
            [
                "appimageupdate",
                "--check-for-update",
                "--",
                "/tmp/file with spaces.AppImage"
            ]
        );
    }
}
