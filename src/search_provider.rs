use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use adw::prelude::*;
use packlift::{
    store::{self, CatalogApp},
    system::{host_access_available, Runner},
};

const PATH: &str = "/io/github/luis_gustavo_git_hub/Packlift/SearchProvider";
const XML: &str = r#"
<node>
  <interface name="org.gnome.Shell.SearchProvider2">
    <method name="GetInitialResultSet">
      <arg type="as" direction="in"/><arg type="as" direction="out"/>
    </method>
    <method name="GetSubsearchResultSet">
      <arg type="as" direction="in"/><arg type="as" direction="in"/><arg type="as" direction="out"/>
    </method>
    <method name="GetResultMetas">
      <arg type="as" direction="in"/><arg type="aa{sv}" direction="out"/>
    </method>
    <method name="ActivateResult">
      <arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="in"/>
    </method>
    <method name="LaunchSearch">
      <arg type="as" direction="in"/><arg type="u" direction="in"/>
    </method>
  </interface>
</node>"#;

#[derive(Default)]
struct Search {
    generation: Cell<u64>,
    results: RefCell<VecDeque<CatalogApp>>,
}

pub(super) fn register(app: &adw::Application) {
    let Some(connection) = app.dbus_connection() else {
        return;
    };
    let interface = gio::DBusNodeInfo::for_xml(XML).and_then(|node| {
        node.lookup_interface("org.gnome.Shell.SearchProvider2")
            .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::Failed, "Missing search interface"))
    });
    let Ok(interface) = interface else {
        return;
    };
    let owner = app.downgrade();
    let search = Rc::new(Search::default());
    let result = connection
        .register_object(PATH, &interface)
        .method_call(move |_, _, _, _, method, arguments, invocation| {
            let Some(app) = owner.upgrade() else {
                invocation
                    .return_dbus_error("org.freedesktop.DBus.Error.Failed", "Packlift closed");
                return;
            };
            match method {
                "GetInitialResultSet" | "GetSubsearchResultSet" => {
                    let terms = arguments.child_value(if method == "GetInitialResultSet" {
                        0
                    } else {
                        1
                    });
                    let query = terms.get::<Vec<String>>().unwrap_or_default().join(" ");
                    query_results(&app, &search, query, invocation);
                }
                "GetResultMetas" => {
                    let ids = arguments
                        .child_value(0)
                        .get::<Vec<String>>()
                        .unwrap_or_default();
                    let results = search.results.borrow();
                    let metas: Vec<HashMap<&str, glib::Variant>> = ids
                        .iter()
                        .filter_map(|id| results.iter().find(|item| item.id == *id))
                        .map(|item| {
                            HashMap::from([
                                ("id", item.id.to_variant()),
                                ("name", item.name.to_variant()),
                                ("description", item.summary.to_variant()),
                                (
                                    "gicon",
                                    gio::ThemedIcon::from_names(&[
                                        &item.id,
                                        "application-x-executable",
                                    ])
                                    .to_string()
                                    .unwrap_or_else(|| "application-x-executable".into())
                                    .to_variant(),
                                ),
                            ])
                        })
                        .collect();
                    invocation.return_value(Some(&(metas,).to_variant()));
                }
                "LaunchSearch" | "ActivateResult" => {
                    let activate = method == "ActivateResult";
                    let terms = arguments.child_value(if activate { 1 } else { 0 });
                    let query = terms.get::<Vec<String>>().unwrap_or_default().join(" ");
                    let item = activate
                        .then(|| arguments.child_value(0).get::<String>())
                        .flatten()
                        .and_then(|id| {
                            search
                                .results
                                .borrow()
                                .iter()
                                .find(|item| item.id == id)
                                .cloned()
                        });
                    crate::build_home(&app);
                    if let Some(window) = app
                        .windows()
                        .into_iter()
                        .filter_map(|window| window.downcast::<adw::ApplicationWindow>().ok())
                        .find(|window| window.transient_for().is_none())
                    {
                        if let Some(item) = item {
                            crate::store_ui::show_search_result(&app, &window, &item);
                        } else {
                            crate::store_ui::show_search(&app, &window, &query);
                        }
                    }
                    invocation.return_value(None);
                }
                _ => {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)
                }
            }
        })
        .build();
    if let Err(error) = result {
        eprintln!("Could not register Packlift search provider: {error}");
    }
}

fn query_results(
    app: &adw::Application,
    search: &Rc<Search>,
    query: String,
    invocation: gio::DBusMethodInvocation,
) {
    let generation = search.generation.get().wrapping_add(1);
    search.generation.set(generation);
    let query = query.trim();
    if query.chars().take(3).count() < 3 || query.len() > 180 {
        invocation.return_value(Some(&(Vec::<String>::new(),).to_variant()));
        return;
    }
    let query = query.to_owned();
    let hold = app.hold();
    let search = search.clone();
    glib::MainContext::default().spawn_local(async move {
        let _hold = hold;
        glib::timeout_future(Duration::from_millis(180)).await;
        if search.generation.get() != generation {
            invocation.return_value(Some(&(Vec::<String>::new(),).to_variant()));
            return;
        }
        let apps = gio::spawn_blocking(move || {
            if !host_access_available() {
                return Vec::new();
            }
            Runner::new(None, Arc::new(|_| {}))
                .and_then(|runner| store::desktop_search(&runner, &query))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        if search.generation.get() != generation {
            invocation.return_value(Some(&(Vec::<String>::new(),).to_variant()));
            return;
        }
        let mut results = search.results.borrow_mut();
        let mut ids = Vec::new();
        for app in apps {
            ids.push(app.id.clone());
            results.retain(|item| item.id != app.id);
            results.push_back(app);
        }
        while results.len() > 96 {
            results.pop_front();
        }
        invocation.return_value(Some(&(ids,).to_variant()));
    });
}
