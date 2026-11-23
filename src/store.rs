use std::{
    collections::{BTreeSet, HashSet},
    io::Read,
    path::PathBuf,
    thread,
};

use serde_json::{json, Value};

use crate::{
    core::{PackliftError, Result},
    system::{host_access_available, operation, Runner},
};

const FLATHUB_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const FLATHUB_API: &str = "https://flathub.org/api/v2";
const PAGE_SIZE: usize = 24;
const SUGGESTION_LIMIT: usize = 6;

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

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum CatalogIcon {
    Flathub(Option<String>),
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

pub struct StorePage {
    pub apps: Vec<(Remote, CatalogApp)>,
    pub has_next: bool,
}

#[derive(Default)]
pub struct AppDetails {
    pub developer: String,
    pub license: String,
    pub description: String,
    pub releases: Vec<Release>,
    pub screenshots: Vec<Screenshot>,
    pub links: Vec<(&'static str, String)>,
    pub category: Option<String>,
    pub free_software: Option<bool>,
    pub adaptive: Option<bool>,
    pub age: Option<u64>,
}

pub struct Release {
    pub version: String,
    pub notes: String,
    pub date: Option<String>,
    pub timestamp: Option<i64>,
}

pub struct Screenshot {
    pub url: String,
    pub large_url: String,
    pub caption: String,
    pub aspect_ratio: Option<f64>,
}

impl Screenshot {
    fn from_sizes(
        sizes: impl Iterator<Item = (u64, u64, String)>,
        caption: String,
    ) -> Option<Self> {
        let sizes: Vec<_> = sizes.collect();
        let nearest = |target| {
            sizes
                .iter()
                .min_by_key(|(width, _, _)| width.abs_diff(target))
        };
        let (width, height, url) = nearest(1100)?;
        Some(Self {
            url: url.clone(),
            large_url: nearest(1920)?.2.clone(),
            caption,
            aspect_ratio: (*width > 0 && *height > 0).then(|| *width as f64 / *height as f64),
        })
    }
}

pub fn remotes(runner: &Runner, destination: Destination) -> Result<Vec<Remote>> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    let mut remotes = configured_remotes(runner, destination, false)?;
    if !remotes.iter().any(|remote| remote.flathub) {
        remotes.push(flathub_remote(destination));
    }
    remotes.sort_by_cached_key(|remote| (!remote.flathub, remote.title.to_lowercase()));
    Ok(remotes)
}

fn flathub_remote(destination: Destination) -> Remote {
    Remote {
        name: String::new(),
        title: "Flathub".into(),
        url: FLATHUB_REPO.into(),
        destination,
        flathub: true,
    }
}

pub fn catalog_remotes(runner: &Runner) -> Result<Vec<Remote>> {
    if !host_access_available() {
        return Err(PackliftError::HostAccessRequired);
    }
    let mut remotes = remotes(runner, Destination::User)
        .unwrap_or_else(|_| vec![flathub_remote(Destination::User)]);
    let system = configured_remotes(runner, Destination::System, false).unwrap_or_default();
    remotes.extend(system.into_iter().filter(|remote| !remote.flathub));
    Ok(remotes)
}

pub struct CatalogSearch {
    query: String,
    remotes: Vec<Remote>,
    remote_page: usize,
    apps: Vec<(Remote, CatalogApp)>,
    seen: HashSet<String>,
    excluded: BTreeSet<String>,
}

impl CatalogSearch {
    pub fn new(remotes: Vec<Remote>, query: String) -> Self {
        Self {
            query,
            remotes,
            remote_page: 1,
            apps: Vec::new(),
            seen: HashSet::new(),
            excluded: BTreeSet::new(),
        }
    }

    pub fn is_explore(&self) -> bool {
        self.query.is_empty()
    }

    pub fn exclude_installed(&mut self, installed: &BTreeSet<String>) {
        if self.is_explore() {
            self.excluded = installed.clone();
        }
    }

    pub fn page(&mut self, runner: &Runner, page: usize) -> Result<StorePage> {
        self.fill_page(page, |remotes, query, remote_page| {
            thread::scope(|workers| {
                let handles: Vec<_> = remotes
                    .iter()
                    .map(|remote| workers.spawn(move || search(runner, remote, query, remote_page)))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                    })
                    .collect()
            })
        })
    }

    fn fill_page(
        &mut self,
        page: usize,
        mut fetch: impl FnMut(&[Remote], &str, usize) -> Vec<Result<CatalogPage>>,
    ) -> Result<StorePage> {
        let page = page.max(1);
        let featured = usize::from(self.is_explore());
        let start = (page - 1) * PAGE_SIZE + if page > 1 { featured } else { 0 };
        let end = page * PAGE_SIZE + featured;
        let first_request = self.remote_page == 1;
        let rotate = self.is_explore() && self.remote_page == 1 && page == 1;
        while !self.remotes.is_empty()
            && (self
                .apps
                .iter()
                .filter(|(_, app)| !self.excluded.contains(&app.id))
                .count()
                <= end
                || (rotate && self.remote_page <= 2))
        {
            let results = fetch(&self.remotes, &self.query, self.remote_page);
            let mut remaining = Vec::new();
            let mut first_error = None;
            let mut loaded = false;
            for (remote, result) in self.remotes.iter().zip(results) {
                match result {
                    Ok(results) => {
                        loaded = true;
                        if results.has_next {
                            remaining.push(remote.clone());
                        }
                        self.apps.extend(results.apps.into_iter().filter_map(|app| {
                            self.seen
                                .insert(app.id.clone())
                                .then(|| (remote.clone(), app))
                        }));
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            if !loaded {
                if let Some(error) = first_error {
                    // A buffered page is still usable when the lookahead fails.
                    if self
                        .apps
                        .iter()
                        .filter(|(_, app)| !self.excluded.contains(&app.id))
                        .count()
                        <= start
                    {
                        return Err(error);
                    }
                    break;
                }
            }
            self.remotes = remaining;
            self.remote_page += 1;
        }
        if !self.is_explore() && page == 1 && first_request {
            let query = self.query.to_lowercase();
            let first_new = if page == 1 { 0 } else { start };
            if let Some(apps) = self.apps.get_mut(first_new..) {
                apps.sort_by_cached_key(|(_, app)| search_rank(app, &query));
            }
        }
        let mut visible = self
            .apps
            .iter()
            .filter(|(_, app)| !self.excluded.contains(&app.id))
            .skip(start);
        Ok(StorePage {
            apps: visible.by_ref().take(end - start).cloned().collect(),
            has_next: visible.next().is_some() || !self.remotes.is_empty(),
        })
    }
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
        let response = if query.is_empty() {
            flathub_collection(runner, "trending", page)
                .or_else(|_| flathub_search(runner, query, page))?
        } else {
            flathub_search(runner, query, page)?
        };
        let apps = response["hits"]
            .as_array()
            .ok_or_else(|| {
                PackliftError::Message("Flathub returned an invalid catalog response.".into())
            })?
            .iter()
            .filter_map(flathub_catalog_app)
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

fn flathub_collection(runner: &Runner, key: &str, page: usize) -> Result<Value> {
    let url = format!("{FLATHUB_API}/collection/{key}?page={page}&per_page={PAGE_SIZE}");
    Ok(serde_json::from_slice(&fetch_with_timeout(
        runner,
        &url,
        None,
        10,
        8 * 1024 * 1024,
    )?)?)
}

pub fn category_apps(
    runner: &Runner,
    category: &str,
    installed: &BTreeSet<String>,
) -> Vec<CatalogApp> {
    let key = format!("category/{category}");
    flathub_collection(runner, &key, 1)
        .ok()
        .into_iter()
        .flat_map(|response| {
            response["hits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(flathub_catalog_app)
                .filter(|app| !installed.contains(&app.id))
                .take(6)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn flathub_search(runner: &Runner, query: &str, page: usize) -> Result<Value> {
    let body = json!({"query": query, "page": page, "hits_per_page": PAGE_SIZE}).to_string();
    Ok(serde_json::from_slice(&fetch(
        runner,
        &format!("{FLATHUB_API}/search"),
        Some(&body),
    )?)?)
}

fn flathub_catalog_app(hit: &Value) -> Option<CatalogApp> {
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
        icon: Some(CatalogIcon::Flathub(
            hit["icon"]
                .as_str()
                .filter(|url| flathub_media(url))
                .map(flathub_icon_hd),
        )),
    })
}

pub fn suggested_apps(
    runner: &Runner,
    remote: &Remote,
    id: &str,
    category: Option<&str>,
) -> Vec<(Remote, CatalogApp)> {
    let mut apps: Vec<_> = category
        .filter(|_| remote.flathub)
        .and_then(|category| similar_apps(runner, id, category).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|app| (remote.clone(), app))
        .collect();
    if apps.len() < SUGGESTION_LIMIT {
        if let Ok(remotes) = catalog_remotes(runner) {
            let mut seen: HashSet<_> = apps.iter().map(|(_, app)| app.id.clone()).collect();
            if let Ok(page) = CatalogSearch::new(remotes, String::new()).page(runner, 1) {
                let remaining = SUGGESTION_LIMIT - apps.len();
                apps.extend(
                    page.apps
                        .into_iter()
                        .filter(|(_, app)| app.id != id && seen.insert(app.id.clone()))
                        .take(remaining),
                );
            }
        }
    }
    apps
}

pub fn similar_apps(runner: &Runner, id: &str, category: &str) -> Result<Vec<CatalogApp>> {
    let response = flathub_search(runner, category, 1)?;
    let mut seen = HashSet::new();
    Ok(response["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|hit| {
            hit["main_categories"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(category))
                || hit["sub_categories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|name| name.as_str() == Some(category))
        })
        .filter_map(flathub_catalog_app)
        .filter(|app| app.id != id && seen.insert(app.id.clone()))
        .take(SUGGESTION_LIMIT)
        .collect())
}

#[derive(Default)]
pub struct FlathubFacts {
    pub download_size: Option<u64>,
    pub installs_last_month: Option<u64>,
    pub permissions: Option<Vec<(&'static str, String)>>,
}

pub fn flathub_facts(runner: &Runner, app: &CatalogApp) -> FlathubFacts {
    if !valid_app_id(&app.id) {
        return FlathubFacts::default();
    }
    let branch = app.reference.rsplit_once('/').map(|(_, branch)| branch);
    let summary_url = format!("{FLATHUB_API}/summary/{}", app.id);
    let summary_url = match branch.filter(|branch| valid_path_component(branch)) {
        Some(branch) => format!("{summary_url}?branch={branch}"),
        None => summary_url,
    };
    let request = |url: &str| -> Option<Value> {
        serde_json::from_slice(&fetch(runner, url, None).ok()?).ok()
    };
    let (summary, stats) = thread::scope(|tasks| {
        let monthly = format!("{FLATHUB_API}/stats/{}", app.id);
        let stats = tasks.spawn(move || request(&monthly));
        (request(&summary_url), stats.join().ok().flatten())
    });
    let permissions = summary.as_ref().and_then(|summary| {
        let permissions = summary["metadata"]["permissions"].as_object()?;
        Some(
            [
                ("Shared resources", "shared"),
                ("Files", "filesystems"),
                ("Devices", "devices"),
                ("Sockets", "sockets"),
                ("Session bus", "session-bus"),
                ("System bus", "system-bus"),
            ]
            .into_iter()
            .filter_map(|(label, key)| {
                let permission = permissions.get(key)?;
                let values = if let Some(values) = permission.as_array() {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                } else if let Some(values) = permission.as_object() {
                    values
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    String::new()
                };
                (!values.is_empty()).then_some((label, values))
            })
            .collect(),
        )
    });
    FlathubFacts {
        download_size: summary
            .as_ref()
            .and_then(|summary| summary["download_size"].as_u64()),
        installs_last_month: stats
            .as_ref()
            .and_then(|stats| stats["installs_last_month"].as_u64()),
        permissions,
    }
}

const APP_LINKS: [(&str, &str); 9] = [
    ("homepage", "Project website"),
    ("vcs_browser", "Source code"),
    ("bugtracker", "Issue tracker"),
    ("contribute", "Contribute"),
    ("help", "Help"),
    ("faq", "FAQ"),
    ("translate", "Translate"),
    ("donation", "Donate"),
    ("contact", "Contact"),
];

fn search_rank(app: &CatalogApp, query: &str) -> u8 {
    let name = app.name.to_lowercase();
    let id = app.id.to_lowercase();
    if name == query || id == query {
        0
    } else if name.starts_with(query) {
        1
    } else if name.contains(query) || id.contains(query) {
        2
    } else {
        3
    }
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
    apps.sort_by_cached_key(|app| (search_rank(app, &query), app.name.to_lowercase()));
    let offset = page.saturating_sub(1) * PAGE_SIZE;
    let has_next = apps.len() > offset + PAGE_SIZE;
    Ok(CatalogPage {
        apps: apps.into_iter().skip(offset).take(PAGE_SIZE).collect(),
        has_next,
    })
}

pub fn app_details(runner: &Runner, remote: &Remote, app: &CatalogApp) -> Result<AppDetails> {
    if !valid_app_id(&app.id) {
        return Ok(AppDetails::default());
    }
    if !remote.flathub {
        return local_app_details(runner, remote, app);
    }
    let metadata: Value = serde_json::from_slice(&fetch(
        runner,
        &format!("{FLATHUB_API}/appstream/{}", app.id),
        None,
    )?)?;
    let screenshots = metadata["screenshots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|shot| {
            let sizes = shot["sizes"].as_array()?;
            let sizes = sizes.iter().filter_map(|size| {
                let url = size["src"].as_str().filter(|url| flathub_media(url))?;
                let dimension = |key: &str| {
                    size[key]
                        .as_u64()
                        .or_else(|| size[key].as_str()?.parse().ok())
                        .unwrap_or(0)
                };
                Some((dimension("width"), dimension("height"), url.to_owned()))
            });
            Screenshot::from_sizes(sizes, shot["caption"].as_str().unwrap_or("").into())
        })
        .take(8)
        .collect();
    let releases = metadata["releases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|release| release["type"].as_str() != Some("development"))
        .filter_map(|release| {
            let version = release["version"].as_str()?.trim();
            (!version.is_empty()).then(|| Release {
                version: version.into(),
                notes: appstream_fragment(release["description"].as_str().unwrap_or_default()),
                date: None,
                timestamp: release["timestamp"]
                    .as_i64()
                    .or_else(|| release["timestamp"].as_str()?.parse().ok()),
            })
        })
        .take(20)
        .collect();
    let mut links = vec![(
        "Flathub page",
        format!("https://flathub.org/apps/{}", app.id),
    )];
    links.extend(APP_LINKS.into_iter().filter_map(|(key, label)| {
        metadata["urls"][key]
            .as_str()
            .filter(|url| https_url(url))
            .map(|url| (label, url.to_owned()))
    }));
    if let Some(url) = metadata["metadata"]["flathub::manifest"]
        .as_str()
        .filter(|url| https_url(url))
    {
        links.push(("Manifest", url.to_owned()));
    } else if metadata["metadata"]["flathub::manifest"].is_null() {
        links.push((
            "Build repository",
            format!("https://github.com/flathub/{}", app.id),
        ));
    }
    Ok(AppDetails {
        developer: metadata["developer_name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .into(),
        license: metadata["project_license"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .into(),
        description: appstream_fragment(metadata["description"].as_str().unwrap_or_default()),
        releases,
        screenshots,
        links,
        category: metadata["categories"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            }),
        free_software: metadata["is_free_license"].as_bool(),
        adaptive: metadata["isMobileFriendly"].as_bool(),
        age: metadata["content_rating_details"]["en_US"]["minimumAge"].as_u64(),
    })
}

fn appstream_fragment(markup: &str) -> String {
    let wrapped = format!("<description>{markup}</description>");
    roxmltree::Document::parse(&wrapped)
        .map(|document| appstream_text(document.root_element()))
        .unwrap_or_default()
}

fn appstream_text(description: roxmltree::Node<'_, '_>) -> String {
    let mut blocks = Vec::new();
    let normalize = |node: roxmltree::Node<'_, '_>| {
        node.descendants()
            .filter(roxmltree::Node::is_text)
            .filter_map(|text| text.text())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    for section in description.children().filter(roxmltree::Node::is_element) {
        if section
            .attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
            .is_some()
        {
            continue;
        }
        match section.tag_name().name() {
            "p" => blocks.push(normalize(section)),
            "ul" | "ol" => {
                let items = section
                    .children()
                    .filter(|item| item.has_tag_name("li"))
                    .map(normalize)
                    .filter(|item| !item.is_empty())
                    .enumerate()
                    .map(|(index, item)| {
                        if section.has_tag_name("ol") {
                            format!("{}. {item}", index + 1)
                        } else {
                            format!("• {item}")
                        }
                    })
                    .collect::<Vec<_>>();
                blocks.push(items.join("\n"));
            }
            _ => {}
        }
    }
    if blocks.is_empty() && !description.children().any(|node| node.is_element()) {
        return normalize(description);
    }
    blocks
        .into_iter()
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn screenshot_bytes(runner: &Runner, url: &str) -> Result<Vec<u8>> {
    if !https_url(url) {
        return Err(PackliftError::Message("Invalid screenshot address.".into()));
    }
    fetch_with_timeout(runner, url, None, 18, 8 * 1024 * 1024)
}

fn flathub_icon_hd(url: &str) -> String {
    url.replace("/icons/128x128/", "/icons/128x128@2/")
        .replace("/icons/64x64/", "/icons/128x128@2/")
}

pub fn catalog_icon_bytes(runner: &Runner, icon: &CatalogIcon, id: &str) -> Option<Vec<u8>> {
    if !valid_app_id(id) {
        return None;
    }
    match icon {
        CatalogIcon::Flathub(url) => {
            let get = |url: &str| {
                flathub_media(url)
                    .then(|| fetch_with_timeout(runner, url, None, 8, 1024 * 1024).ok())
                    .flatten()
            };
            if let Some(url) = url {
                if let Some(icon) = get(url) {
                    return Some(icon);
                }
                let normal = url.replace("/icons/128x128@2/", "/icons/128x128/");
                if normal != *url {
                    if let Some(icon) = get(&normal)
                        .or_else(|| get(&normal.replace("/icons/128x128/", "/icons/64x64/")))
                    {
                        return Some(icon);
                    }
                }
            }
            let metadata = fetch_with_timeout(
                runner,
                &format!("{FLATHUB_API}/appstream/{id}"),
                None,
                8,
                2 * 1024 * 1024,
            )
            .ok()?;
            let metadata: Value = serde_json::from_slice(&metadata).ok()?;
            let icon = metadata["icon"].as_str()?;
            let hd = flathub_icon_hd(icon);
            get(&hd).or_else(|| (hd != icon).then(|| get(icon)).flatten())
        }
        CatalogIcon::AppStream(directory) => ["128x128@2", "128x128", "64x64@2", "64x64"]
            .iter()
            .find_map(|size| {
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

fn local_app_details(runner: &Runner, remote: &Remote, app: &CatalogApp) -> Result<AppDetails> {
    let arch = app.reference.split('/').nth(2).unwrap_or_default();
    if !valid_path_component(arch) {
        return Ok(AppDetails::default());
    }
    let Some(root) = appstream_root(runner, remote)? else {
        return Ok(AppDetails::default());
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
        return Ok(AppDetails::default());
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
    details_from_appstream(&xml, app)
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

fn details_from_appstream(xml: &str, app: &CatalogApp) -> Result<AppDetails> {
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
    let Some(component) = component else {
        return Ok(AppDetails::default());
    };
    let field = |name| component.children().find(|node| node.has_tag_name(name));
    let screenshots = component
        .descendants()
        .filter(|node| node.has_tag_name("screenshot"))
        .filter_map(|shot| {
            let sizes = shot
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
                    let dimension = |key| {
                        image
                            .attribute(key)
                            .and_then(|value| value.parse::<u64>().ok())
                            .unwrap_or(0)
                    };
                    Some((dimension("width"), dimension("height"), url))
                });
            let caption = shot
                .children()
                .find(|node| node.has_tag_name("caption"))
                .and_then(|node| node.text())
                .unwrap_or_default()
                .into();
            Screenshot::from_sizes(sizes, caption)
        })
        .take(8)
        .collect();
    let developer = field("developer")
        .and_then(|node| {
            node.children()
                .find(|child| child.has_tag_name("name"))
                .and_then(|child| child.text())
        })
        .or_else(|| field("developer_name").and_then(|node| node.text()));
    let releases = field("releases")
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| {
            node.has_tag_name("release") && node.attribute("type") != Some("development")
        })
        .filter_map(|node| {
            let version = node.attribute("version")?.trim();
            (!version.is_empty()).then(|| Release {
                version: version.into(),
                date: node.attribute("date").map(str::to_owned),
                timestamp: node
                    .attribute("timestamp")
                    .and_then(|value| value.parse().ok()),
                notes: node
                    .children()
                    .find(|child| child.has_tag_name("description"))
                    .map(appstream_text)
                    .unwrap_or_default(),
            })
        })
        .take(20)
        .collect();
    let links = component
        .children()
        .filter(|node| node.has_tag_name("url"))
        .filter_map(|node| {
            let key = node.attribute("type")?;
            let (_, label) = APP_LINKS.iter().find(|(candidate, _)| *candidate == key)?;
            let url = node.text()?.trim();
            https_url(url).then(|| (*label, url.to_owned()))
        })
        .collect();
    Ok(AppDetails {
        developer: developer.unwrap_or_default().trim().into(),
        license: field("project_license")
            .and_then(|node| node.text())
            .unwrap_or_default()
            .trim()
            .into(),
        description: field("description").map(appstream_text).unwrap_or_default(),
        releases,
        screenshots,
        links,
        ..AppDetails::default()
    })
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
    fetch_with_timeout(runner, url, post, 18, 2 * 1024 * 1024)
}

fn fetch_with_timeout(
    runner: &Runner,
    url: &str,
    post: Option<&str>,
    seconds: u32,
    max_bytes: u64,
) -> Result<Vec<u8>> {
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
        return runner.capture_bytes_limited(&args, false, max_bytes);
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
        seconds.to_string(),
        "--max-filesize".into(),
        max_bytes.to_string(),
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
    runner.capture_bytes_limited(&args, false, max_bytes)
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
    fn installed_apps_are_excluded_from_explore_but_match_searches() {
        let installed = BTreeSet::from(["org.example.Installed".to_owned()]);
        let remote = flathub_remote(Destination::User);
        let fetch = |_: &[Remote], _: &str, _: usize| {
            vec![search_remote_list(
                "org.example.Installed/x86_64/stable\tInstalled\norg.example.Other/x86_64/stable\tOther\n",
                "",
                1,
            )]
        };
        let mut explore = CatalogSearch::new(vec![remote.clone()], String::new());
        explore.exclude_installed(&installed);
        let results = explore.fill_page(1, fetch).unwrap();
        assert_eq!(results.apps.len(), 1);
        assert_eq!(results.apps[0].1.id, "org.example.Other");

        let mut search = CatalogSearch::new(vec![remote], "Installed".into());
        search.exclude_installed(&installed);
        let results = search.fill_page(1, fetch).unwrap();
        assert!(results
            .apps
            .iter()
            .any(|(_, app)| app.id == "org.example.Installed"));
    }

    #[test]
    fn featured_app_does_not_leave_a_gap_in_the_grid() {
        let mut catalog = CatalogSearch::new(
            vec![
                flathub_remote(Destination::User),
                flathub_remote(Destination::System),
            ],
            String::new(),
        );
        let mut requests = Vec::new();
        let mut fetch = |remotes: &[Remote], _: &str, remote_page| {
            requests.push((remotes.len(), remote_page));
            let rows = (0..PAGE_SIZE)
                .map(|index| {
                    let id = format!("org.example.App{}", (remote_page - 1) * PAGE_SIZE + index);
                    format!("{id}/x86_64/stable\t{id}\n")
                })
                .collect::<String>();
            remotes
                .iter()
                .map(|remote| {
                    let mut result = search_remote_list(&rows, "", 1)?;
                    result.has_next = remote.destination == Destination::User && remote_page < 2;
                    Ok(result)
                })
                .collect()
        };

        let first = catalog.fill_page(1, &mut fetch).unwrap();
        let second = catalog.fill_page(2, &mut fetch).unwrap();
        let back = catalog.fill_page(1, &mut fetch).unwrap();
        assert_eq!(first.apps.len(), PAGE_SIZE + 1);
        assert!(first.has_next);
        assert_eq!(second.apps.len(), PAGE_SIZE - 1);
        assert!(!second.has_next);
        assert_eq!(back.apps.len(), first.apps.len());
        let ids = first
            .apps
            .iter()
            .chain(&second.apps)
            .map(|(_, app)| &app.id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), PAGE_SIZE * 2);
        assert_eq!(requests, [(2, 1), (1, 2)]);
    }

    #[test]
    fn appstream_details_match_the_selected_repo_branch() {
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
            <developer><name>Example team</name></developer><project_license>MIT</project_license>
            <description><p>A test &amp; example.</p><ul><li>First feature</li></ul></description>
            <releases><release version="2.0" type="development"/><release version="1.0">
               <description><p>First release</p></description>
            </release></releases>
            <screenshots><screenshot><caption>App window</caption><image width="752" height="376">stable.png</image>
                <image width="1600">https://example.net/big.png</image></screenshot></screenshots>
          </component></components>"#;
        let details = details_from_appstream(xml, &app).unwrap();
        assert_eq!(details.developer, "Example team");
        assert_eq!(details.license, "MIT");
        assert_eq!(details.description, "A test & example.\n\n• First feature");
        let release = &details.releases[0];
        assert_eq!(release.version, "1.0");
        assert_eq!(release.notes, "First release");
        let shots = details.screenshots;
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].url, "https://example.org/media/stable.png");
        assert_eq!(shots[0].large_url, "https://example.net/big.png");
        assert_eq!(shots[0].aspect_ratio, Some(2.0));
        assert_eq!(shots[0].caption, "App window");
    }
}
