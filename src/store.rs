use std::{io::Read, path::PathBuf};

use serde_json::{json, Value};

use crate::{
    core::{PackliftError, Result},
    system::{host_access_available, operation, Runner},
};

const FLATHUB_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const FLATHUB_API: &str = "https://flathub.org/api/v2";
const PAGE_SIZE: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    User,
    System,
}

impl Destination {
    pub fn flag(self) -> &'static str {
        match self {
            Self::User => "--user",
            Self::System => "--system",
        }
    }
}

#[derive(Clone)]
pub struct Remote {
    pub name: String,
    pub title: String,
    pub url: String,
    pub destination: Destination,
    pub flathub: bool,
}

impl Remote {
    pub fn label(&self) -> String {
        if self.name.is_empty() || self.name.eq_ignore_ascii_case(&self.title) {
            self.title.clone()
        } else {
            format!("{} · {}", self.name, self.title)
        }
    }
}

#[derive(Clone)]
pub enum CatalogIcon {
    Flathub(String),
    AppStream(PathBuf),
}

#[derive(Clone)]
pub struct CatalogApp {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub reference: String,
    pub icon: Option<CatalogIcon>,
}

pub struct CatalogPage {
    pub apps: Vec<CatalogApp>,
    pub has_next: bool,
}

pub struct Screenshot {
    pub url: String,
    pub caption: String,
}

pub fn remotes(runner: &Runner, destination: Destination) -> Result<Vec<Remote>> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    let mut remotes = configured_remotes(runner, destination, false)?;
    if !remotes.iter().any(|remote| remote.flathub) {
        remotes.push(Remote {
            name: String::new(),
            title: "Flathub".into(),
            url: FLATHUB_REPO.into(),
            destination,
            flathub: true,
        });
    }
    remotes.sort_by_cached_key(|remote| (!remote.flathub, remote.title.to_lowercase()));
    Ok(remotes)
}

pub fn install_choices(
    runner: &Runner,
    source: &Remote,
    app: &CatalogApp,
    destination: Destination,
) -> Vec<(Remote, CatalogApp)> {
    let mut remotes = remotes(runner, destination).unwrap_or_default();
    if source.destination == destination
        && !remotes
            .iter()
            .any(|remote| remote.name == source.name && remote.url == source.url)
    {
        remotes.insert(0, source.clone());
    }
    remotes.sort_by_key(|remote| !(remote.url == source.url && remote.name == source.name));
    remotes
        .into_iter()
        .filter_map(|remote| {
            let same_source = remote.destination == source.destination
                && remote.name == source.name
                && remote.url == source.url;
            let reference = if same_source && !remote.flathub {
                app.reference.clone()
            } else if remote.flathub && remote.name.is_empty() {
                if !source.flathub {
                    let metadata =
                        fetch(runner, &format!("{FLATHUB_API}/appstream/{}", app.id), None).ok()?;
                    let metadata: Value = serde_json::from_slice(&metadata).ok()?;
                    if metadata["id"] != app.id {
                        return None;
                    }
                }
                app.id.clone()
            } else {
                let output = runner
                    .capture(
                        &[
                            "flatpak".into(),
                            "remote-info".into(),
                            destination.flag().into(),
                            "--app".into(),
                            "--show-ref".into(),
                            "--".into(),
                            remote.name.clone(),
                            app.id.clone(),
                        ],
                        false,
                    )
                    .ok()?;
                let reference = output.trim();
                let mut parts = reference.split('/');
                if parts.next() != Some("app")
                    || parts.next() != Some(app.id.as_str())
                    || !parts.next().is_some_and(valid_path_component)
                    || !parts.next().is_some_and(valid_path_component)
                    || parts.next().is_some()
                {
                    return None;
                }
                reference.into()
            };
            Some((
                remote,
                CatalogApp {
                    reference,
                    ..app.clone()
                },
            ))
        })
        .collect()
}

fn configured_remotes(
    runner: &Runner,
    destination: Destination,
    include_disabled: bool,
) -> Result<Vec<Remote>> {
    let mut args = vec![
        "flatpak".into(),
        "remotes".into(),
        destination.flag().into(),
        "--columns=name,url,title".into(),
    ];
    if include_disabled {
        args.push("--show-disabled".into());
    }
    let output = match runner.capture(&args, false) {
        Ok(output) => output,
        Err(error) if destination == Destination::System => {
            if let Some(root) = system_installation_path(runner)? {
                let check = runner.run(
                    &[
                        "test".into(),
                        "-d".into(),
                        root.join("repo").to_string_lossy().into_owned(),
                    ],
                    false,
                    true,
                    false,
                )?;
                if !check.status.success() {
                    return Ok(Vec::new());
                }
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let name = fields.next().filter(|name| !name.is_empty())?;
            let url = fields.next()?;
            let title = fields
                .next()
                .filter(|title| !title.is_empty() && *title != "-")
                .unwrap_or(name);
            let flathub = matches!(
                url.trim_end_matches('/'),
                "https://dl.flathub.org/repo" | "https://flathub.org/repo"
            );
            Some(Remote {
                name: name.into(),
                title: title.into(),
                url: url.into(),
                destination,
                flathub,
            })
        })
        .collect())
}

pub fn search(runner: &Runner, remote: &Remote, query: &str, page: usize) -> Result<CatalogPage> {
    if remote.flathub {
        let response = fetch(
            runner,
            &format!("{FLATHUB_API}/search"),
            Some(&json!({"query":query,"page":page,"hits_per_page":PAGE_SIZE}).to_string()),
        )?;
        let response: Value = serde_json::from_slice(&response)?;
        let hits = response["hits"].as_array().ok_or_else(|| {
            PackliftError::Message("Flathub returned an invalid catalog response.".into())
        })?;
        let apps = hits
            .iter()
            .filter_map(|hit| {
                let id = hit["app_id"].as_str().filter(|id| valid_app_id(id))?;
                let name = hit["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id);
                Some(CatalogApp {
                    id: id.into(),
                    name: name.into(),
                    summary: hit["summary"].as_str().unwrap_or_default().into(),
                    reference: id.into(),
                    icon: hit["icon"]
                        .as_str()
                        .filter(|url| flathub_media(url))
                        .map(|url| CatalogIcon::Flathub(url.into())),
                })
            })
            .collect();
        return Ok(CatalogPage {
            apps,
            has_next: response["totalPages"].as_u64().unwrap_or(0) > page as u64,
        });
    }
    let output = runner.capture(
        &[
            "flatpak".into(),
            "remote-ls".into(),
            remote.destination.flag().into(),
            "--app".into(),
            "--columns=ref,name,description".into(),
            "--".into(),
            remote.name.clone(),
        ],
        false,
    )?;
    let mut results = search_remote_list(&output, query, page)?;
    if let Ok(Some(root)) = appstream_root(runner, remote) {
        for app in &mut results.apps {
            if let Some(arch) = app.reference.split('/').nth(2) {
                app.icon = Some(CatalogIcon::AppStream(root.join(arch).join("active/icons")));
            }
        }
    }
    Ok(results)
}

fn search_remote_list(output: &str, query: &str, page: usize) -> Result<CatalogPage> {
    let query = query.to_lowercase();
    let mut apps: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let reference = fields.next()?.trim_start_matches("app/");
            let mut parts = reference.split('/');
            let id = parts.next().filter(|id| valid_app_id(id))?;
            let arch = parts.next()?;
            let branch = parts.next()?;
            if !valid_path_component(arch)
                || !valid_path_component(branch)
                || parts.next().is_some()
            {
                return None;
            }
            let name = fields.next().filter(|name| !name.is_empty()).unwrap_or(id);
            let summary = fields.next().unwrap_or_default();
            format!("{name} {id} {summary} {branch}")
                .to_lowercase()
                .contains(&query)
                .then(|| CatalogApp {
                    id: id.into(),
                    name: name.into(),
                    summary: summary.into(),
                    reference: format!("app/{reference}"),
                    icon: None,
                })
        })
        .collect();
    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    let offset = page.saturating_sub(1) * PAGE_SIZE;
    let has_next = apps.len() > offset + PAGE_SIZE;
    Ok(CatalogPage {
        apps: apps.into_iter().skip(offset).take(PAGE_SIZE).collect(),
        has_next,
    })
}

pub fn screenshots(runner: &Runner, remote: &Remote, app: &CatalogApp) -> Result<Vec<Screenshot>> {
    if !valid_app_id(&app.id) {
        return Ok(Vec::new());
    }
    if !remote.flathub {
        return local_screenshots(runner, remote, app);
    }
    let metadata: Value = serde_json::from_slice(&fetch(
        runner,
        &format!("{FLATHUB_API}/appstream/{}", app.id),
        None,
    )?)?;
    Ok(metadata["screenshots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|shot| {
            let sizes = shot["sizes"].as_array()?;
            let url = sizes
                .iter()
                .filter_map(|size| {
                    let url = size["src"].as_str().filter(|url| flathub_media(url))?;
                    let width = size["width"]
                        .as_str()
                        .and_then(|width| width.parse::<u32>().ok())
                        .unwrap_or(0);
                    Some((width.abs_diff(752), url))
                })
                .min_by_key(|(distance, _)| *distance)?
                .1;
            Some(Screenshot {
                url: url.into(),
                caption: shot["caption"].as_str().unwrap_or("").into(),
            })
        })
        .take(8)
        .collect())
}

pub fn screenshot_bytes(runner: &Runner, url: &str) -> Result<Vec<u8>> {
    if !https_url(url) {
        return Err(PackliftError::Message("Invalid screenshot address.".into()));
    }
    fetch(runner, url, None)
}

pub fn catalog_icon_bytes(runner: &Runner, icon: &CatalogIcon, id: &str) -> Option<Vec<u8>> {
    if !valid_app_id(id) {
        return None;
    }
    match icon {
        CatalogIcon::Flathub(url) if flathub_media(url) => fetch(runner, url, None).ok(),
        CatalogIcon::Flathub(_) => None,
        CatalogIcon::AppStream(directory) => ["64x64", "128x128"].iter().find_map(|size| {
            let path = directory.join(size).join(format!("{id}.png"));
            runner
                .capture_bytes_limited(
                    &[
                        "cat".into(),
                        "--".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                    false,
                    256 * 1024,
                )
                .ok()
        }),
    }
}

fn appstream_root(runner: &Runner, remote: &Remote) -> Result<Option<PathBuf>> {
    if !valid_path_component(&remote.name) {
        return Ok(None);
    }
    let installation = match remote.destination {
        Destination::User => std::env::var_os("FLATPAK_USER_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(crate::system::data_home().join("flatpak"))),
        Destination::System => system_installation_path(runner)?,
    };
    Ok(installation.map(|path| path.join("appstream").join(&remote.name)))
}

fn local_screenshots(
    runner: &Runner,
    remote: &Remote,
    app: &CatalogApp,
) -> Result<Vec<Screenshot>> {
    let arch = app.reference.split('/').nth(2).unwrap_or_default();
    if !valid_path_component(arch) {
        return Ok(Vec::new());
    }
    let Some(root) = appstream_root(runner, remote)? else {
        return Ok(Vec::new());
    };
    let path = root.join(arch).join("active/appstream.xml.gz");
    const COMPRESSED_LIMIT: u64 = 8 * 1024 * 1024;
    const XML_LIMIT: u64 = 32 * 1024 * 1024;
    let Ok(bytes) = runner.capture_bytes_limited(
        &[
            "cat".into(),
            "--".into(),
            path.to_string_lossy().into_owned(),
        ],
        false,
        COMPRESSED_LIMIT,
    ) else {
        return Ok(Vec::new());
    };
    let mut xml = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(XML_LIMIT + 1)
        .read_to_string(&mut xml)?;
    if xml.len() as u64 > XML_LIMIT {
        return Err(PackliftError::Message(
            "The AppStream catalog is too large.".into(),
        ));
    }
    screenshots_from_appstream(&xml, app)
}

fn system_installation_path(runner: &Runner) -> Result<Option<PathBuf>> {
    let paths = runner.capture(&["flatpak".into(), "--installations".into()], false)?;
    let mut paths = paths.lines().filter(|path| path.starts_with('/'));
    Ok(paths.next().map(|first| {
        PathBuf::from(if first == "/var/lib/flatpak" {
            first
        } else {
            paths
                .find(|path| *path == "/var/lib/flatpak")
                .unwrap_or(first)
        })
    }))
}

fn screenshots_from_appstream(xml: &str, app: &CatalogApp) -> Result<Vec<Screenshot>> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| PackliftError::Message(format!("Invalid AppStream metadata: {error}")))?;
    let base = document
        .root_element()
        .attribute("media_baseurl")
        .filter(|base| https_url(base));
    let component = document
        .descendants()
        .filter(|node| node.has_tag_name("component"))
        .find(|component| {
            let mut fields = component
                .children()
                .filter(|node| {
                    node.has_tag_name("bundle") && node.attribute("type") == Some("flatpak")
                })
                .peekable();
            if fields.peek().is_some() {
                fields.any(|node| node.text().map(str::trim) == Some(app.reference.as_str()))
            } else {
                component.children().any(|node| {
                    node.has_tag_name("id")
                        && node
                            .text()
                            .is_some_and(|id| id.trim().trim_end_matches(".desktop") == app.id)
                })
            }
        });
    Ok(component
        .into_iter()
        .flat_map(|component| component.descendants())
        .filter(|node| node.has_tag_name("screenshot"))
        .filter_map(|shot| {
            let url = shot
                .children()
                .filter(|node| node.has_tag_name("image"))
                .filter_map(|image| {
                    let text = image.text()?.trim();
                    let url = if https_url(text) {
                        text.to_owned()
                    } else if !text.is_empty()
                        && !text.contains(':')
                        && !text.starts_with("//")
                        && !text.contains(char::is_control)
                    {
                        format!(
                            "{}/{}",
                            base?.trim_end_matches('/'),
                            text.trim_start_matches('/')
                        )
                    } else {
                        return None;
                    };
                    let width = image
                        .attribute("width")
                        .and_then(|width| width.parse::<u32>().ok())
                        .unwrap_or(0);
                    Some((width.abs_diff(752), url))
                })
                .min_by_key(|(distance, _)| *distance)?
                .1;
            let caption = shot
                .children()
                .find(|node| node.has_tag_name("caption"))
                .and_then(|node| node.text())
                .unwrap_or_default()
                .into();
            Some(Screenshot { url, caption })
        })
        .take(8)
        .collect())
}

pub fn install(runner: &Runner, remote: &Remote, app: &CatalogApp) -> Result<()> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    if !valid_app_id(&app.id) {
        return Err(PackliftError::Message(
            "Invalid Flatpak application ID.".into(),
        ));
    }
    let _operation = operation(&runner.data)?;
    let flag = remote.destination.flag();
    let name = if remote.name.is_empty() && remote.flathub {
        let available = configured_remotes(runner, remote.destination, false)?;
        if let Some(flathub) = available.into_iter().find(|remote| remote.flathub) {
            flathub.name
        } else {
            let configured = configured_remotes(runner, remote.destination, true)?;
            let name = (1..)
                .map(|counter| {
                    if counter == 1 {
                        "flathub".into()
                    } else {
                        format!("flathub-{counter}")
                    }
                })
                .find(|name| !configured.iter().any(|remote| remote.name == *name))
                .unwrap();
            runner.run(
                &[
                    "flatpak".into(),
                    "remote-add".into(),
                    flag.into(),
                    "--".into(),
                    name.clone(),
                    FLATHUB_REPO.into(),
                ],
                false,
                true,
                true,
            )?;
            name
        }
    } else {
        if !configured_remotes(runner, remote.destination, false)?
            .iter()
            .any(|configured| configured.name == remote.name && configured.url == remote.url)
        {
            return Err(PackliftError::Message(
                "The selected Flatpak repository changed. Open the store again.".into(),
            ));
        }
        remote.name.clone()
    };
    runner.run(
        &[
            "flatpak".into(),
            "install".into(),
            flag.into(),
            "--app".into(),
            "--assumeyes".into(),
            "--".into(),
            name,
            app.reference.clone(),
        ],
        false,
        true,
        true,
    )?;
    Ok(())
}

fn fetch(runner: &Runner, url: &str, post: Option<&str>) -> Result<Vec<u8>> {
    if !runner.available("curl") {
        if !runner.available("wget") {
            return Err(PackliftError::Message(
                "The store requires curl or wget on the host.".into(),
            ));
        }
        let mut args = vec![
            "wget".into(),
            "--no-config".into(),
            "--https-only".into(),
            "--max-redirect=0".into(),
            "--timeout=8".into(),
            "--tries=1".into(),
            "--quiet".into(),
            "-O".into(),
            "-".into(),
        ];
        if let Some(post) = post {
            args.extend([
                "--header=Content-Type: application/json".into(),
                format!("--post-data={post}"),
            ]);
        }
        args.extend(["--".into(), url.into()]);
        return runner.capture_bytes(&args, false);
    }
    let mut args = vec![
        "curl".into(),
        "--disable".into(),
        "--fail".into(),
        "--silent".into(),
        "--show-error".into(),
        "--proto".into(),
        "=https".into(),
        "--connect-timeout".into(),
        "6".into(),
        "--max-time".into(),
        "18".into(),
        "--max-filesize".into(),
        "2097152".into(),
    ];
    if let Some(post) = post {
        args.extend([
            "--header".into(),
            "Content-Type: application/json".into(),
            "--data-binary".into(),
            post.into(),
        ]);
    }
    args.extend(["--".into(), url.into()]);
    runner.capture_bytes(&args, false)
}

fn valid_app_id(id: &str) -> bool {
    id.split('.').count() >= 3
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        })
}

fn flathub_media(url: &str) -> bool {
    url.starts_with("https://dl.flathub.org/media/") && https_url(url)
}

fn https_url(url: &str) -> bool {
    url.starts_with("https://") && !url.contains(char::is_control)
}

fn valid_path_component(component: &str) -> bool {
    !component.is_empty()
        && !matches!(component, "." | "..")
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_remote_results_keep_the_exact_branch_and_architecture() {
        let text = "org.example.App/x86_64/stable\tExample App\tA test editor\norg.example.Other/x86_64/beta\tOther\tOther app\n";
        let page = search_remote_list(text, "editor", 1).unwrap();
        assert_eq!(page.apps.len(), 1);
        assert_eq!(page.apps[0].reference, "app/org.example.App/x86_64/stable");
        assert!(!page.has_next);
    }

    #[test]
    fn appstream_screenshots_match_the_selected_repo_branch() {
        let app = CatalogApp {
            id: "org.example.App".into(),
            name: "Example".into(),
            summary: String::new(),
            reference: "app/org.example.App/x86_64/stable".into(),
            icon: None,
        };
        let xml = r#"<components media_baseurl="https://example.org/media/">
          <component><id>org.example.App</id><bundle type="flatpak">app/org.example.App/x86_64/beta</bundle>
            <screenshots><screenshot><image width="752">beta.png</image></screenshot></screenshots>
          </component><component><id>org.example.App</id><bundle type="flatpak">app/org.example.App/x86_64/stable</bundle>
            <screenshots><screenshot><caption>App window</caption><image width="752">stable.png</image>
                <image width="1280">https://example.net/big.png</image></screenshot></screenshots>
          </component></components>"#;
        let shots = screenshots_from_appstream(xml, &app).unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].url, "https://example.org/media/stable.png");
        assert_eq!(shots[0].caption, "App window");
    }
}
