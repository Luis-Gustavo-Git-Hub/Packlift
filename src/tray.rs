use std::{cell::RefCell, rc::Rc};

use gio::prelude::*;
use glib::variant::ToVariant;

use crate::APP_ID;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";

const ITEM_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="i" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionMovieName" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
  </interface>
</node>
"#;

struct TrayHandle {
    _connection: gio::DBusConnection,
    _registration: gio::RegistrationId,
}

thread_local! {
    static TRAY: RefCell<Option<TrayHandle>> = const { RefCell::new(None) };
}

fn item_property(name: &str) -> Option<glib::Variant> {
    let empty_pixmaps: Vec<(i32, i32, Vec<u8>)> = Vec::new();
    Some(match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => "Packlift".to_variant(),
        "Title" => "Packlift".to_variant(),
        "Status" => "Active".to_variant(),
        "WindowId" => 0_i32.to_variant(),
        "IconName" => APP_ID.to_variant(),
        "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => empty_pixmaps.to_variant(),
        "OverlayIconName" | "AttentionIconName" | "AttentionMovieName" => "".to_variant(),
        "ToolTip" => (
            APP_ID,
            empty_pixmaps,
            "Packlift".to_owned(),
            "Background update checks are active".to_owned(),
        )
            .to_variant(),
        "ItemIsMenu" => false.to_variant(),
        "Menu" => "/".to_variant(),
        _ => return None,
    })
}

fn open_packlift() {
    let Some(info) = gio::DesktopAppInfo::new(&format!("{APP_ID}.desktop")) else {
        return;
    };
    let _ = info.launch(&[], None::<&gio::AppLaunchContext>);
}

fn register_with_watcher(connection: &gio::DBusConnection) -> bool {
    connection
        .call_sync(
            Some(WATCHER),
            WATCHER_PATH,
            WATCHER,
            "RegisterStatusNotifierItem",
            Some(&(ITEM_PATH,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            1_000,
            None::<&gio::Cancellable>,
        )
        .is_ok()
}

pub(super) fn start() {
    if TRAY.with_borrow(|tray| tray.is_some()) {
        return;
    }
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return;
    };
    let Ok(node) = gio::DBusNodeInfo::for_xml(ITEM_XML) else {
        return;
    };
    let Some(interface) = node.lookup_interface(ITEM_INTERFACE) else {
        return;
    };
    let registration = connection
        .register_object(ITEM_PATH, &interface)
        .method_call(|_, _, _, _, method, _, invocation| {
            if matches!(method, "Activate" | "SecondaryActivate") {
                open_packlift();
            }
            invocation.return_value(None);
        })
        .property(|_, _, _, _, name| item_property(name).unwrap_or_else(|| ().to_variant()))
        .build();
    let Ok(registration) = registration else {
        return;
    };

    let retry_connection = connection.clone();
    let registered = Rc::new(std::cell::Cell::new(register_with_watcher(&connection)));
    let retry_state = registered.clone();
    glib::timeout_add_seconds_local(15, move || {
        if retry_state.get() {
            return glib::ControlFlow::Break;
        }
        if register_with_watcher(&retry_connection) {
            retry_state.set(true);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    TRAY.with_borrow_mut(|tray| {
        *tray = Some(TrayHandle {
            _connection: connection,
            _registration: registration,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_notifier_item_xml_has_a_valid_interface() {
        let node = gio::DBusNodeInfo::for_xml(ITEM_XML).unwrap();
        let interface = node.lookup_interface(ITEM_INTERFACE).unwrap();
        assert!(interface.lookup_method("Activate").is_some());
        assert!(interface.lookup_method("ContextMenu").is_some());
        assert!(interface.lookup_property("IconName").is_some());
        assert!(interface.lookup_property("Status").is_some());
    }

    #[test]
    fn tray_icon_reports_packlift_as_active() {
        assert_eq!(
            item_property("IconName")
                .unwrap()
                .get::<String>()
                .as_deref(),
            Some(APP_ID)
        );
        assert_eq!(
            item_property("Status").unwrap().get::<String>().as_deref(),
            Some("Active")
        );
        assert_eq!(
            item_property("ToolTip").unwrap().type_().as_str(),
            "(sa(iiay)ss)"
        );
    }
}
