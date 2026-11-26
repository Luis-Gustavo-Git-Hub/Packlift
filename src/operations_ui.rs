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

use crate::{home_label, i18n::tr, installation_stage, text_button};

type Task = Box<dyn FnOnce(LogHandler) -> Result<Option<serde_json::Value>> + Send>;
type Complete = Box<dyn FnOnce(Result<Option<serde_json::Value>>)>;
type Changed = Box<dyn Fn()>;

struct Operation {
    id: u64,
    title: String,
    phase: Phase,
    fraction: f64,
    message: String,
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
) {
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
            fraction: 0.0,
            message: String::new(),
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
    phase(id, |entry| {
        entry.phase = Phase::Running;
        entry.fraction = 0.04;
    });
    let (sender, receiver) = mpsc::channel();
    let worker = gio::spawn_blocking(move || {
        task(Arc::new(move |line| {
            let progress = installation_stage(line);
            let _ = sender.send(progress);
        }))
    });
    let timer = glib::timeout_add_local(Duration::from_millis(160), move || {
        let progress = receiver.try_iter().last();
        if let Some((stage, fraction)) = progress {
            let changed = HISTORY.with_borrow(|history| {
                history.iter().any(|entry| {
                    entry.id == id && (entry.message != stage || entry.fraction < fraction)
                })
            });
            if changed {
                phase(id, |entry| {
                    entry.message = stage.to_owned();
                    entry.fraction = entry.fraction.max(fraction);
                });
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
                entry.message.clear();
                entry.fraction = 1.0;
            }
            Err(error) => {
                entry.phase = Phase::Failed;
                entry.message = error.to_string();
            }
        });
        completed(result);
        released();
        ACTIVE.with(|active| active.set(false));
        start_next();
    });
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

pub(super) fn panel() -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    panel.append(&list);
    rebuild(&list);
    let weak = list.downgrade();
    observe(&panel, move || {
        if let Some(list) = weak.upgrade() {
            rebuild(&list);
        }
    });
    panel
}

fn rebuild(list: &gtk::Box) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    HISTORY.with_borrow(|history| {
        if history.is_empty() {
            list.append(&home_label(tr("No recent operations"), "dim-label"));
        }
        let active = history
            .iter()
            .filter(|entry| matches!(entry.phase, Phase::Running | Phase::Queued));
        let recent = history
            .iter()
            .rev()
            .filter(|entry| !matches!(entry.phase, Phase::Running | Phase::Queued));
        for entry in active.chain(recent) {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
            card.add_css_class("packlift-update-section");
            card.append(&home_label(&entry.title, "heading"));
            let status = match entry.phase {
                Phase::Queued => tr("Queued"),
                Phase::Running if entry.message.is_empty() => tr("Working…"),
                Phase::Running => &entry.message,
                Phase::Done => tr("Completed"),
                Phase::Failed => tr("Failed"),
                Phase::Canceled => tr("Canceled"),
            };
            card.append(&home_label(status, "dim-label"));
            if entry.phase == Phase::Running {
                let bar = gtk::ProgressBar::new();
                bar.set_fraction(entry.fraction);
                bar.update_property(&[gtk::accessible::Property::Label(&entry.title)]);
                card.append(&bar);
            }
            if entry.phase == Phase::Failed {
                let details = gtk::Expander::new(Some(tr("Details")));
                let message = home_label(&entry.message, "error");
                message.set_selectable(true);
                details.set_child(Some(&message));
                card.append(&details);
            }
            if entry.phase == Phase::Queued {
                let button = text_button(tr("Cancel"));
                button.set_halign(gtk::Align::End);
                let id = entry.id;
                button.connect_clicked(move |_| cancel(id));
                card.append(&button);
            }
            list.append(&card);
        }
    });
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
