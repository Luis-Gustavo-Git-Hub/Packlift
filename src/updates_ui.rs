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
    Flatpaks,
    AppImages,
}

#[derive(Clone)]
enum Backend {
    Flatpak(&'static str, Vec<String>),
    Flatpaks(Vec<(&'static str, Vec<String>)>),
    AppImages(String, Vec<(std::path::PathBuf, bool)>),
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

fn command(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

const UPDATE_INTERVAL_DAYS: [u64; 4] = [0, 1, 7, 30];

thread_local! {
    static AUTO_UPDATE_TIMER_STARTED: Cell<bool> = const { Cell::new(false) };
    static AUTO_CHECK_TIMER_STARTED: Cell<bool> = const { Cell::new(false) };
    static AUTO_UPDATE_RUNNING: Cell<bool> = const { Cell::new(false) };
    static UPDATE_SECTIONS: RefCell<Vec<std::rc::Weak<Section>>> = const { RefCell::new(Vec::new()) };
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

fn write_schedule(days: u64, last_run: u64) {
    let mut state = update_state();
    state["interval_days"] = serde_json::json!(days);
    state["last_auto_update"] = serde_json::json!(last_run);
    save_update_state(state);
}

fn check_key(source: Source) -> &'static str {
    match source {
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
        (0, runtimes) => tr("{count} runtime updates available").replace("{count}", &runtimes.to_string()),
        (count, runtimes) => tr("{count} apps and {runtimes} runtime updates available")
            .replace("{count}", &count.to_string())
            .replace("{runtimes}", &runtimes.to_string()),
    }
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
        let sections = UPDATE_SECTIONS.with_borrow_mut(|registered| {
            let sections = registered.iter().filter_map(std::rc::Weak::upgrade).collect::<Vec<_>>();
            registered.retain(|section| section.strong_count() > 0);
            sections
        });
        for section in sections {
            section.refresh();
        }
        glib::ControlFlow::Continue
    });
}

fn distribution_identity() -> (String, String) {
    let result = Runner::new(None, Arc::new(|_| {}))
        .and_then(|runner| runner.capture(&command(&["cat", "/etc/os-release"]), false));
    let Ok(contents) = result else {
        return (tr("Linux distribution").to_owned(), "computer-symbolic".to_owned());
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

fn apply_due_updates() -> Result<()> {
    let mut errors = Vec::new();
    let sources = if host_access_available() {
        vec![Source::Flatpaks, Source::AppImages]
    } else {
        vec![Source::AppImages]
    };
    for source in sources {
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
        if AUTO_UPDATE_RUNNING.with(Cell::get) {
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
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    match source {
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
        let path = data.join("installed").join(id).join("program.AppImage");
        if !path.is_file() {
            let sandbox_only = app["sandbox_only"].as_bool() == Some(true);
            let migrated = sandbox_only
                && app["source"]
                    .as_str()
                    .map(|source| std::path::Path::new(source))
                    .filter(|source| source.is_file())
                    .is_some_and(|source| fs::copy(source, &path).is_ok());
            if !migrated {
                unsupported += if sandbox_only { 1 } else { 0 };
                continue;
            }
        }
        let path_text = path.to_string_lossy().into_owned();
        let describe = runner.capture(
            &command(&[updater, "--describe", "--", &path_text]),
            local,
        )?;
        if !describe.lines().any(|line| {
            line.trim_start()
                .strip_prefix("Update info:")
                .is_some_and(|info| !info.trim().is_empty())
        })
        {
            unsupported += 1;
            continue;
        }
        supported += 1;

        runner.write(&format!("Checking AppImage updates for {}", path.display()));
        let result = std::process::Command::new(updater)
            .args(["--check-for-update", "--", path_text.as_str()])
            .output()?;
        let diagnostic = String::from_utf8_lossy(&result.stderr);
        for line in String::from_utf8_lossy(&result.stdout)
            .lines()
            .chain(diagnostic.lines())
        {
            runner.write(line);
        }
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
                updates.push((path, app["sandbox_only"].as_bool() == Some(true)));
                names.push(
                    app["name"]
                        .as_str()
                        .unwrap_or("AppImage")
                        .to_owned(),
                );
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
        return Err(PackliftError::Message(
            failed_remotes.join("\n"),
        ));
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

fn apply(backend: &Backend) -> Result<()> {
    let _operation = operation(&packlift_data_dir())?;
    let runner = Runner::new(None, Arc::new(|_| {}))?;
    match backend {
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
            for (path, sandbox_only) in paths {
                let path_text = path.to_string_lossy().into_owned();
                let result = runner.run(
                    &command(&[updater, "--overwrite", "--", &path_text]),
                    true,
                    true,
                    false,
                )?;
                if !result.status.success() {
                    return Err(PackliftError::Message(format!(
                        "{} could not be updated. See the Packlift log.",
                        path.file_name().and_then(|name| name.to_str()).unwrap_or("AppImage")
                    )));
                }
                if *sandbox_only {
                    if let Err(error) = refresh_sandbox_appimage(&runner, path) {
                        restore_appimage_backup(path);
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(())
}

fn refresh_sandbox_appimage(runner: &Runner, image: &std::path::Path) -> Result<()> {
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
        group.add_css_class("packlift-update-section");
        if matches!(source, Source::Flatpaks) {
            group.add_css_class("packlift-update-flatpak");
        }
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::from_icon_name(icon_name);
        icon.set_pixel_size(if matches!(source, Source::Flatpaks) { 40 } else { 18 });
        if matches!(source, Source::Flatpaks) {
            icon.add_css_class("packlift-flatpak-update-icon");
        }
        heading.append(&icon);
        heading.append(&home_label(title, "heading"));
        group.append(&heading);
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
        let section = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let source = section.source;
            let result = gio::spawn_blocking(move || check(source)).await;
            section.busy.set(false);
            let checked_at = record_last_check(source, now_epoch());
            section.last_checked.set_text(&checked_at);
            match result {
                Ok(Ok(result)) => {
                    *section.backend.borrow_mut() = result.available.then_some(result.backend.clone());
                    let partial = !result.failed_remotes.is_empty();
                    let unsupported = matches!(section.source, Source::AppImages)
                        && result.unsupported > 0;
                    section.warning.set_visible(partial || unsupported);
                    let mut warnings = Vec::new();
                    if partial {
                        warnings.push(if matches!(section.source, Source::Flatpaks) {
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
                    section.warning.set_text(&warnings.join("\n"));
                    let status = if result.available && matches!(section.source, Source::Flatpaks) {
                        format_flatpak_status(&result)
                    } else if result.available {
                        match section.source {
                            Source::AppImages => {
                                let count = result.packages.len();
                                let label = if count == 1 {
                                    tr("1 AppImage update available").to_owned()
                                } else {
                                    tr("{count} AppImage updates available")
                                        .replace("{count}", &count.to_string())
                                };
                                label
                            }
                            Source::Flatpaks => tr("Updates available").to_owned(),
                        }
                    } else if partial {
                        tr("Update check incomplete").to_owned()
                    } else if matches!(section.source, Source::AppImages)
                        && result.supported > 0
                    {
                        tr("All supported AppImages are up to date.").to_owned()
                    } else if matches!(section.source, Source::AppImages) {
                        tr("No AppImages with automatic update support were found.").to_owned()
                    } else {
                        String::new()
                    };
                    section.status.set_text(&status);
                    section.status.set_visible(
                        result.available
                            || partial
                            || matches!(section.source, Source::AppImages),
                    );
                    let mut lines = result.packages.iter().take(8).cloned().collect::<Vec<_>>();
                    if result.packages.len() > 8 {
                        lines.push(
                            tr("And {count} more")
                                .replace("{count}", &(result.packages.len() - 8).to_string()),
                        );
                    }
                    section.details.set_text(&lines.join("\n"));
                    section.details.set_visible(
                        !lines.is_empty() && matches!(section.source, Source::AppImages),
                    );
                    section.update.set_visible(
                        result.available && matches!(section.source, Source::AppImages),
                    );
                    section.last_checked.set_visible(true);
                }
                Ok(Err(error)) => {
                    *section.backend.borrow_mut() = None;
                    section.update.set_visible(false);
                    section.last_checked.set_visible(true);
                    section.status.set_text(&error.to_string());
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
            let result = gio::spawn_blocking(move || apply(&backend)).await;
            section.busy.set(false);
            section.update.set_sensitive(true);
            match result {
                Ok(Ok(())) => {
                    *section.backend.borrow_mut() = None;
                    section.update.set_visible(false);
                    section.details.set_visible(false);
                    section.refresh();
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
    let sections = [flatpaks, appimages];
    register_update_sections(&sections);
    ensure_auto_check_timer();
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
