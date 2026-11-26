use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use serde_json::Value;
use soup::prelude::*;
use webkit6::prelude::*;

use crate::{home_label, i18n::tr, text_button};
use glib::translate::IntoGlib;

const ACCOUNT_URL: &str = "https://flathub.org/my-flathub";
const API: &str = "https://flathub.org/api/v2";

thread_local! {
    static SESSION: RefCell<Option<soup::Session>> = const { RefCell::new(None) };
}

pub(super) fn session() -> Option<soup::Session> {
    SESSION.with_borrow(Clone::clone)
}

pub(super) fn sign_out() {
    SESSION.with_borrow_mut(|session| {
        if let Some(session) = session.take() {
            session.abort();
        }
    });
}

#[derive(Debug)]
pub(super) enum Error {
    SignedOut,
    Request,
}

impl Error {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::SignedOut => tr("Your Flathub session expired. Sign in again."),
            Self::Request => tr("Could not connect to Flathub. Try again."),
        }
    }
}

async fn request(session: &soup::Session, method: &str, route: &str) -> Result<Value, Error> {
    let message =
        soup::Message::new(method, &format!("{API}{route}")).map_err(|_| Error::Request)?;
    message.set_flags(soup::MessageFlags::NO_REDIRECT);
    let bytes = session
        .send_and_read_future(&message, glib::Priority::DEFAULT)
        .await
        .map_err(|_| Error::Request)?;
    let status = message.status();
    if status == soup::Status::Unauthorized || status == soup::Status::Forbidden {
        if self::session().as_ref() == Some(session) {
            sign_out();
        }
        return Err(Error::SignedOut);
    }
    if !(200..300).contains(&status.into_glib()) {
        return Err(Error::Request);
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Request)
}

pub(super) async fn favorites(session: &soup::Session) -> Result<Vec<String>, Error> {
    let apps = request(session, "GET", "/favorites").await?;
    let apps = apps.as_array().ok_or(Error::Request)?;
    let mut ids = apps
        .iter()
        .filter_map(|app| app["app_id"].as_str())
        .filter(|id| valid_app_id(id))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

pub(super) async fn app_info(session: &soup::Session, id: String) -> packlift::store::CatalogApp {
    use packlift::store::{CatalogApp, CatalogIcon};

    let metadata = request(session, "GET", &format!("/appstream/{id}"))
        .await
        .unwrap_or_default();
    CatalogApp {
        name: metadata["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(&id)
            .to_owned(),
        summary: metadata["summary"].as_str().unwrap_or_default().to_owned(),
        icon: Some(CatalogIcon::Flathub(None)),
        reference: id.clone(),
        id,
    }
}

pub(super) async fn is_favorite(session: &soup::Session, id: &str) -> Result<bool, Error> {
    if !valid_app_id(id) {
        return Err(Error::Request);
    }
    request(session, "GET", &format!("/favorites/{id}"))
        .await?
        .as_bool()
        .ok_or(Error::Request)
}

pub(super) async fn set_favorite(
    session: &soup::Session,
    id: &str,
    saved: bool,
) -> Result<(), Error> {
    if !valid_app_id(id) {
        return Err(Error::Request);
    }
    let (method, action) = if saved {
        ("POST", "add")
    } else {
        ("DELETE", "remove")
    };
    request(session, method, &format!("/favorites/{id}/{action}")).await?;
    Ok(())
}

fn valid_app_id(id: &str) -> bool {
    id.split('.').count() >= 3
        && !id.split('.').any(str::is_empty)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(super) fn sign_in(owner: &adw::ApplicationWindow, connected: impl Fn() + 'static) {
    let window = adw::Window::builder()
        .title(tr("Sign in to Flathub"))
        .default_width(760)
        .default_height(690)
        .transient_for(owner)
        .modal(true)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new(tr("Sign in to Flathub"), "flathub.org");
    header.set_title_widget(Some(&title));
    toolbar.add_top_bar(&header);

    let network = webkit6::NetworkSession::new_ephemeral();
    let Some(cookies) = network.cookie_manager() else {
        crate::show_error(owner, tr("Could not connect to Flathub. Try again."));
        return;
    };
    let web = webkit6::WebView::builder()
        .network_session(&network)
        .build();
    let address = title.downgrade();
    web.connect_uri_notify(move |web| {
        if let Some(title) = address.upgrade() {
            let uri = web
                .uri()
                .and_then(|uri| glib::Uri::parse(&uri, glib::UriFlags::NONE).ok());
            let host = uri.as_ref().and_then(glib::Uri::host);
            title.set_subtitle(host.as_deref().unwrap_or_default());
        }
    });
    web.connect_decide_policy(|_, decision, kind| {
        if kind == webkit6::PolicyDecisionType::NewWindowAction {
            decision.ignore();
            return true;
        }
        let uri = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|decision| decision.navigation_action())
            .and_then(|mut action| action.request())
            .and_then(|request| request.uri());
        if uri.is_some_and(|uri| !uri.starts_with("https://")) {
            decision.ignore();
            return true;
        }
        false
    });
    web.connect_permission_request(|_, request| {
        request.deny();
        true
    });
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    web.set_vexpand(true);
    content.append(&web);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.set_margin_top(10);
    actions.set_margin_bottom(10);
    actions.set_margin_start(12);
    actions.set_margin_end(12);
    let status = home_label("", "dim-label");
    status.set_hexpand(true);
    actions.append(&status);
    let connect = text_button(tr("Connect favorites"));
    connect.add_css_class("suggested-action");
    connect.set_sensitive(false);
    actions.append(&connect);
    content.append(&actions);
    toolbar.set_content(Some(&content));
    window.set_content(Some(&toolbar));

    let ready = Rc::new(Cell::new(false));
    let load_ready = ready.clone();
    let button = connect.downgrade();
    web.connect_load_changed(move |web, event| {
        let on_account = event == webkit6::LoadEvent::Finished
            && web
                .uri()
                .and_then(|uri| glib::Uri::parse(&uri, glib::UriFlags::NONE).ok())
                .is_some_and(|uri| {
                    uri.scheme() == "https"
                        && uri.host().as_deref() == Some("flathub.org")
                        && matches!(uri.port(), -1 | 443)
                        && uri.path().ends_with("/my-flathub")
                });
        load_ready.set(on_account);
        if let Some(button) = button.upgrade() {
            button.set_sensitive(on_account);
        }
    });

    let weak_window = window.downgrade();
    let connected = Rc::new(connected);
    connect.connect_clicked(move |button| {
        if !ready.replace(false) {
            return;
        }
        button.set_sensitive(false);
        let (window, status, button) =
            (weak_window.clone(), status.downgrade(), button.downgrade());
        let (cookies, connected, ready) = (cookies.clone(), connected.clone(), ready.clone());
        glib::MainContext::default().spawn_local(async move {
            let result = async {
                let url = format!("{API}/favorites");
                let page_cookies = cookies
                    .cookies_future(&url)
                    .await
                    .map_err(|_| Error::Request)?;
                let jar = soup::CookieJar::new();
                for mut cookie in page_cookies {
                    if cookie.name().as_deref() == Some("session")
                        && cookie.domain_matches("flathub.org")
                    {
                        jar.add_cookie(&mut cookie);
                    }
                }
                let session = soup::Session::builder().timeout(12).build();
                session.add_feature(&jar);
                favorites(&session).await?;
                Ok::<_, Error>(session)
            }
            .await;
            let Some(window) = window.upgrade().filter(|window| window.is_visible()) else {
                return;
            };
            match result {
                Ok(session) => {
                    SESSION.with_borrow_mut(|current| *current = Some(session));
                    connected();
                    window.close();
                }
                Err(error) => {
                    if let Some(status) = status.upgrade() {
                        status.set_text(error.label());
                    }
                    if let Some(button) = button.upgrade() {
                        ready.set(true);
                        button.set_sensitive(true);
                    }
                }
            }
        });
    });
    web.load_uri(ACCOUNT_URL);
    window.present();
}
