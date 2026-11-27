use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{mpsc, Arc},
    time::Duration,
};

use adw::prelude::*;
use packlift::{
    core::{PackliftError, Result},
    system::LogHandler,
};

use crate::{home_label, i18n::tr, text_button};

type Task = Box<dyn FnOnce(LogHandler) -> Result<Option<serde_json::Value>> + Send>;
type Complete = Box<dyn FnOnce(Result<Option<serde_json::Value>>)>;
type Changed = Box<dyn Fn()>;

struct Operation {
    id: u64,
    title: String,
    phase: Phase,
    progress: Progress,
    error: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Done,
    Failed,
    Canceled,
}

struct Pending {
    id: u64,
    task: Task,
    completed: Complete,
    released: Box<dyn FnOnce()>,
}

thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static PENDING: RefCell<VecDeque<Pending>> = const { RefCell::new(VecDeque::new()) };
    static HISTORY: RefCell<Vec<Operation>> = const { RefCell::new(Vec::new()) };
    static LISTENERS: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Rc<Changed>)>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn observe(widget: &impl IsA<gtk::Widget>, callback: impl Fn() + 'static) {
    LISTENERS.with_borrow_mut(|listeners| {
        listeners.retain(|(owner, _)| owner.upgrade().is_some());
        listeners.push((
            widget.upcast_ref::<gtk::Widget>().downgrade(),
            Rc::new(Box::new(callback)),
        ));
    });
}

fn notify() {
    let callbacks = LISTENERS.with_borrow_mut(|listeners| {
        listeners.retain(|(owner, _)| owner.upgrade().is_some());
        listeners
            .iter()
            .map(|(_, callback)| callback.clone())
            .collect::<Vec<_>>()
    });
    for callback in callbacks {
        callback();
    }
}

fn phase(id: u64, action: impl FnOnce(&mut Operation)) {
    HISTORY.with_borrow_mut(|history| {
        if let Some(entry) = history.iter_mut().find(|entry| entry.id == id) {
            action(entry);
        }
    });
    notify();
}

pub(super) fn submit(
    app: &gtk::Application,
    title: &str,
    task: impl FnOnce(LogHandler) -> Result<Option<serde_json::Value>> + Send + 'static,
    completed: impl FnOnce(Result<Option<serde_json::Value>>) + 'static,
    released: impl FnOnce() + 'static,
) -> u64 {
    let id = NEXT.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let hold = app.hold();
    HISTORY.with_borrow_mut(|history| {
        history.retain(|entry| {
            entry.id.saturating_add(30) >= id
                || matches!(entry.phase, Phase::Queued | Phase::Running)
        });
        history.push(Operation {
            id,
            title: title.to_owned(),
            phase: Phase::Queued,
            progress: Progress::PREPARING,
            error: String::new(),
        });
    });
    PENDING.with_borrow_mut(|queue| {
        queue.push_back(Pending {
            id,
            task: Box::new(task),
            completed: Box::new(completed),
            released: Box::new(move || {
                released();
                drop(hold);
            }),
        })
    });
    notify();
    start_next();
    id
}

fn start_next() {
    if ACTIVE.with(Cell::get) {
        return;
    }
    let Some(Pending {
        id,
        task,
        completed,
        released,
    }) = PENDING.with_borrow_mut(VecDeque::pop_front)
    else {
        return;
    };
    ACTIVE.with(|active| active.set(true));
    phase(id, |entry| entry.phase = Phase::Running);
    let (sender, receiver) = mpsc::channel();
    let worker = gio::spawn_blocking(move || {
        task(Arc::new(move |line| {
            if let Some(progress) = describe_progress(line) {
                let _ = sender.send(progress);
            }
        }))
    });
    let timer = glib::timeout_add_local(Duration::from_millis(160), move || {
        if let Some(progress) = receiver.try_iter().last() {
            let changed = HISTORY.with_borrow(|history| {
                history
                    .iter()
                    .any(|entry| entry.id == id && entry.progress != progress)
            });
            if changed {
                phase(id, |entry| entry.progress = progress);
            }
        }
        glib::ControlFlow::Continue
    });
    glib::MainContext::default().spawn_local(async move {
        let result = worker.await.unwrap_or_else(|_| {
            Err(PackliftError::Message(
                tr("The operation ended unexpectedly.").into(),
            ))
        });
        timer.remove();
        phase(id, |entry| match &result {
            Ok(_) => {
                entry.phase = Phase::Done;
            }
            Err(error) => {
                entry.phase = Phase::Failed;
                entry.error = error.to_string();
            }
        });
        completed(result);
        released();
        ACTIVE.with(|active| active.set(false));
        start_next();
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    stage: &'static str,
    explanation: &'static str,
    item: Option<(u32, u32)>,
    percent: Option<u8>,
}

impl Progress {
    const PREPARING: Self = Self::stage(
        "Preparing the operation",
        "Packlift is getting everything ready.",
    );

    const fn stage(stage: &'static str, explanation: &'static str) -> Self {
        Self {
            stage,
            explanation,
            item: None,
            percent: None,
        }
    }
}

fn describe_progress(line: &str) -> Option<Progress> {
    let line = line.trim();
    // Flatpak's non-terminal output reports progress for one component at a time.
    for (verb, stage, explanation) in [
        (
            "Installing",
            "Installing Flatpak components",
            "Downloading and deploying the app and any required runtimes.",
        ),
        (
            "Updating",
            "Updating Flatpak components",
            "Downloading and applying new versions of apps and runtimes.",
        ),
        (
            "Uninstalling",
            "Removing a Flatpak component",
            "Removing the selected installation from Flatpak.",
        ),
    ] {
        if let Some((head, tail)) = line
            .strip_prefix(verb)
            .and_then(|rest| rest.split_once('…').or_else(|| rest.split_once("...")))
        {
            let item = if head.trim().is_empty() {
                None
            } else {
                let (current, total) = head.trim().split_once('/')?;
                let (current, total) = (current.parse::<u32>().ok()?, total.parse::<u32>().ok()?);
                if current == 0 || current > total {
                    return None;
                }
                Some((current, total))
            };
            let percent = tail
                .split_whitespace()
                .find_map(|word| word.strip_suffix('%')?.parse::<u8>().ok())
                .filter(|value| *value <= 100);
            return Some(Progress {
                item,
                percent,
                ..Progress::stage(stage, explanation)
            });
        }
    }

    for (prefix, stage, explanation) in [
        (
            "Preparing Flatpak installation",
            "Preparing Flatpak installation",
            "Checking the selected repository and resolving dependencies.",
        ),
        (
            "Checking Flatpak updates",
            "Checking for Flatpak updates",
            "Looking for new versions in the configured repositories.",
        ),
        (
            "Updating Flatpaks for my account",
            "Updating Flatpaks for my account",
            "Checking and applying updates in your personal installation.",
        ),
        (
            "Updating system Flatpaks",
            "Updating system Flatpaks",
            "Checking and applying updates in the shared installation.",
        ),
        (
            "Refreshing Flatpak state",
            "Finishing the installation",
            "Checking that the app is available to open.",
        ),
        (
            "Checking AppImage updates",
            "Checking for AppImage updates",
            "Looking for a newer version of the installed file.",
        ),
        (
            "Applying AppImage update",
            "Updating AppImage files",
            "Downloading and replacing the installed file.",
        ),
        (
            "Analyzing package",
            "Analyzing the package",
            "Reading the package metadata and identifying its format.",
        ),
        (
            "Checking package compatibility",
            "Checking compatibility",
            "Checking the package architecture and how it can run on this system.",
        ),
        (
            "Preparing the ",
            "Preparing the installation environment",
            "Setting up the runtime or container required to run the app.",
        ),
        (
            "Downloading package dependencies",
            "Downloading dependencies",
            "Fetching files the app needs to run.",
        ),
        (
            "Running the Windows installer",
            "Running the Windows installer",
            "Follow the instructions in the installer if it asks for input.",
        ),
        (
            "Looking for the installed Windows executable",
            "Finding the installed program",
            "Looking for the executable to add to your library.",
        ),
        (
            "Creating application menu",
            "Creating application shortcuts",
            "Adding the app to your desktop menu.",
        ),
        (
            "Registering the application",
            "Adding the app to your library",
            "Saving the information Packlift needs to open the app.",
        ),
        (
            "Removing the installed program",
            "Removing the program",
            "Removing the installation and its shortcuts.",
        ),
        (
            "Installing",
            "Installing the program",
            "Copying the program files to the selected installation.",
        ),
    ] {
        if line.starts_with(prefix) {
            return Some(Progress::stage(stage, explanation));
        }
    }
    None
}

fn cancel(id: u64) {
    let pending = PENDING.with_borrow_mut(|queue| {
        let index = queue.iter().position(|entry| entry.id == id)?;
        queue.remove(index)
    });
    if let Some(pending) = pending {
        phase(id, |entry| entry.phase = Phase::Canceled);
        (pending.released)();
    }
}

pub(super) fn cancel_queued(title: &str) {
    let id = HISTORY.with_borrow(|history| {
        history
            .iter()
            .find(|entry| entry.title == title && entry.phase == Phase::Queued)
            .map(|entry| entry.id)
    });
    if let Some(id) = id {
        cancel(id);
    }
}

pub(super) fn panel() -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    panel.append(&list);
    let mut layout = None;
    let mut running = None;
    let weak = list.downgrade();
    let refresh = move || {
        let Some(list) = weak.upgrade() else {
            return;
        };
        HISTORY.with_borrow(|history| {
            let active = history
                .iter()
                .filter(|entry| matches!(entry.phase, Phase::Running | Phase::Queued));
            let recent = history
                .iter()
                .rev()
                .filter(|entry| !matches!(entry.phase, Phase::Running | Phase::Queued));
            let entries = active.chain(recent).collect::<Vec<_>>();
            let next = entries
                .iter()
                .map(|entry| (entry.id, entry.phase))
                .collect::<Vec<_>>();
            if layout.as_ref() != Some(&next) {
                running = rebuild(&list, &entries);
                layout = Some(next);
            }
            if let (Some(widgets), Some(entry)) = (
                &running,
                entries.iter().find(|entry| entry.phase == Phase::Running),
            ) {
                widgets.update(entry.progress);
            }
        });
    };
    let refresh = Rc::new(RefCell::new(refresh));
    (refresh.borrow_mut())();
    observe(&panel, move || (refresh.borrow_mut())());
    panel
}

pub(super) fn track(slot: &gtk::Box, id: u64, title: &str) {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 7);
    let card = RunningCard::new(&container, title);
    slot.append(&container);
    let container_ref = container.downgrade();
    let refresh = move || {
        if container_ref.upgrade().is_none() {
            return;
        }
        HISTORY.with_borrow(|history| {
            let Some(entry) = history.iter().find(|entry| entry.id == id) else {
                return;
            };
            let progress = match entry.phase {
                Phase::Queued => Progress::stage(
                    "In the operations queue",
                    "Installation will start when the current operation finishes.",
                ),
                Phase::Running => entry.progress,
                _ => return,
            };
            card.update(progress);
        });
    };
    refresh();
    observe(&container, refresh);
}

struct RunningCard {
    stage: gtk::Label,
    explanation: gtk::Label,
    spinner: gtk::Spinner,
    measurement: gtk::Label,
    bar: gtk::ProgressBar,
}

impl RunningCard {
    fn new(card: &gtk::Box, title: &str) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        row.append(&spinner);
        let stage = home_label("", "heading");
        row.append(&stage);
        card.append(&row);
        let explanation = home_label("", "dim-label");
        card.append(&explanation);
        let measurement = home_label("", "caption");
        measurement.add_css_class("dim-label");
        card.append(&measurement);
        let bar = gtk::ProgressBar::new();
        bar.update_property(&[gtk::accessible::Property::Label(title)]);
        card.append(&bar);
        Self {
            stage,
            explanation,
            spinner,
            measurement,
            bar,
        }
    }

    fn update(&self, progress: Progress) {
        self.stage.set_text(tr(progress.stage));
        self.explanation.set_text(tr(progress.explanation));
        let mut details = Vec::new();
        if let Some((current, total)) = progress.item {
            details.push(
                tr("Component {current} of {total}")
                    .replace("{current}", &current.to_string())
                    .replace("{total}", &total.to_string()),
            );
        }
        if let Some(percent) = progress.percent {
            details.push(
                tr("{percent}% of this component").replace("{percent}", &percent.to_string()),
            );
            self.bar.set_fraction(f64::from(percent) / 100.0);
        }
        self.measurement.set_text(&details.join(" · "));
        self.measurement.set_visible(!details.is_empty());
        self.bar.set_visible(progress.percent.is_some());
        self.spinner.set_visible(progress.percent.is_none());
        self.spinner.set_spinning(progress.percent.is_none());
    }
}

fn rebuild(list: &gtk::Box, entries: &[&Operation]) -> Option<RunningCard> {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    if entries.is_empty() {
        list.append(&home_label(tr("No recent operations"), "dim-label"));
    }
    let mut running = None;
    let mut waiting = 0;
    for entry in entries {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("packlift-update-section");
        card.append(&home_label(&entry.title, "heading"));
        match entry.phase {
            Phase::Running => running = Some(RunningCard::new(&card, &entry.title)),
            Phase::Queued => {
                waiting += 1;
                let status = tr("Waiting in queue · Position {position}")
                    .replace("{position}", &waiting.to_string());
                card.append(&home_label(&status, "dim-label"));
                let button = text_button(tr("Cancel"));
                button.set_halign(gtk::Align::End);
                let id = entry.id;
                button.connect_clicked(move |_| cancel(id));
                card.append(&button);
            }
            Phase::Done => card.append(&home_label(tr("Completed"), "dim-label")),
            Phase::Canceled => card.append(&home_label(tr("Canceled"), "dim-label")),
            Phase::Failed => {
                card.append(&home_label(tr("Failed"), "error"));
                let details = gtk::Expander::new(Some(tr("Details")));
                let message = home_label(&entry.error, "error");
                message.set_selectable(true);
                details.set_child(Some(&message));
                card.append(&details);
            }
        }
        list.append(&card);
    }
    running
}

pub(super) fn has_active_title(title: &str) -> bool {
    HISTORY.with_borrow(|history| {
        history.iter().any(|entry| {
            entry.title == title && matches!(entry.phase, Phase::Running | Phase::Queued)
        })
    })
}

pub(super) fn active_count() -> usize {
    HISTORY.with_borrow(|history| {
        history
            .iter()
            .filter(|entry| matches!(entry.phase, Phase::Running | Phase::Queued))
            .count()
    })
}

pub(super) fn attach_button(header: &adw::HeaderBar) {
    let button = gtk::MenuButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text(tr("Operations"))
        .build();
    let popover = gtk::Popover::new();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(280)
        .max_content_height(460)
        .propagate_natural_height(true)
        .child(&panel())
        .build();
    popover.set_child(Some(&scroll));
    button.set_popover(Some(&popover));
    header.pack_end(&button);
}
