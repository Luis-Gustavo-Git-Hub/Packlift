use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{
    core::{architecture_group, detect, Package, PackliftError, Result},
    system::{
        cancellation_requested, desktop_exec, export_desktop, export_desktop_with_wm_class,
        host_access_available, operation, packlift_data_dir, remove_exports, sha256,
        sha256_with_progress, Registry, Runner,
    },
    APP_ID,
};

const FLATPAK_WINE: &str = "org.winehq.Wine//wow64-25.08";
type RpmHeaderIndex = HashMap<u32, (u32, u32, u32)>;
type SystemLauncher = (String, Vec<String>, String, bool, String);

#[derive(Clone)]
pub struct Installer {
    pub data: PathBuf,
    pub applications: PathBuf,
    pub runner: Runner,
    pub registry: Registry,
}

/// Optional launch overrides for a Windows program in the library.
pub struct WindowsLaunchOptions {
    pub working_directory: Option<PathBuf>,
    pub arguments: String,
    pub environment: String,
    pub disable_dxvk: bool,
    pub disable_vkd3d: bool,
    pub disable_esync: bool,
    pub disable_fsync: bool,
}

impl Installer {
    pub fn new(
        output: crate::system::LogHandler,
        data: Option<PathBuf>,
        applications: Option<PathBuf>,
    ) -> Result<Self> {
        let data = data.unwrap_or_else(packlift_data_dir);
        let applications =
            applications.unwrap_or_else(|| crate::system::data_home().join("applications"));
        let runner = Runner::new(Some(data.clone()), output)?;
        let registry = Registry::new(&data)?;
        let installer = Self {
            data,
            applications,
            runner,
            registry,
        };
        if host_access_available() {
            installer.refresh_application_menu_entries();
        }
        Ok(installer)
    }

    pub fn set_default_file_handler(&self) -> Result<()> {
        const HANDLER_ENTRY: &str =
            include_str!("../data/io.github.luis_gustavo_git_hub.Packlift.desktop");
        const MIME_DEFINITIONS: &str =
            include_str!("../data/io.github.luis_gustavo_git_hub.Packlift.xml");

        let handler = format!("{APP_ID}.desktop");
        let _operation = operation(&self.data)?;
        for executable in ["gio", "xdg-mime", "update-mime-database"] {
            if !self.runner.available(executable) {
                return Err(PackliftError::Message(format!(
                    "The host needs {executable} to set default applications."
                )));
            }
        }
        let mime_file = self
            .data
            .join("io.github.luis_gustavo_git_hub.Packlift.xml");
        fs::write(&mime_file, MIME_DEFINITIONS)?;
        self.runner.run(
            &[
                "xdg-mime".into(),
                "install".into(),
                "--mode".into(),
                "user".into(),
                "--novendor".into(),
                mime_file.to_string_lossy().into_owned(),
            ],
            false,
            true,
            true,
        )?;

        let types = HANDLER_ENTRY
            .lines()
            .find_map(|line| line.strip_prefix("MimeType="))
            .ok_or_else(|| {
                PackliftError::Message("Packlift has no registered file types.".into())
            })?;
        let mut failed = Vec::new();
        for mime in types.split(';').filter(|mime| !mime.is_empty()) {
            if self
                .runner
                .run(
                    &["gio".into(), "mime".into(), mime.into(), handler.clone()],
                    false,
                    true,
                    true,
                )
                .is_err()
            {
                failed.push(mime);
            }
        }
        if !failed.is_empty() {
            return Err(PackliftError::Message(format!(
                "Could not set the default for {}. See the Packlift log for details.",
                failed.join(", ")
            )));
        }
        Ok(())
    }

    pub fn preferences(&self) -> (String, String) {
        let path = self.data.join("preferences.json");
        let Ok(value) = fs::read_to_string(path) else {
            return ("wine".into(), String::new());
        };
        let Ok(value) = serde_json::from_str::<Value>(&value) else {
            return ("wine".into(), String::new());
        };
        (
            value["runtime"].as_str().unwrap_or("wine").into(),
            String::new(),
        )
    }

    pub fn save_preferences(&self, runtime: &str) -> Result<()> {
        if runtime != "wine" && runtime != "proton" {
            return Err(PackliftError::Message("Choose Wine or Proton.".into()));
        }
        let temporary = self.data.join("preferences.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec(&json!({"runtime": runtime, "proton_path": ""}))?,
        )?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(temporary, self.data.join("preferences.json"))?;
        Ok(())
    }

    pub fn probe(&self, path: &Path) -> Result<(Package, crate::core::Plan)> {
        self.probe_with_progress(path, |_, _| {})
    }

    pub fn probe_with_progress(
        &self,
        path: &Path,
        progress: impl FnMut(u64, u64),
    ) -> Result<(Package, crate::core::Plan)> {
        self.runner
            .write(&format!("Analyzing package: {}", path.display()));
        let (kind, architecture) = detect(path)?;
        let sandbox_only = !host_access_available();
        if sandbox_only && !matches!(kind.as_str(), "appimage" | "tar" | "binary") {
            return Err(PackliftError::HostAccessRequired);
        }
        if sandbox_only && kind == "appimage" && appimage_offset(path)?.is_none() {
            return Err(PackliftError::Message(
                "Sandbox mode requires a type 2 AppImage.".into(),
            ));
        }
        let resolved_path = if path.is_dir() {
            find_dir_binary(path).unwrap_or_else(|| path.to_owned())
        } else {
            path.to_owned()
        };
        let mut package = Package {
            path: resolved_path.clone(),
            kind: kind.clone(),
            name: resolved_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Program")
                .to_owned(),
            architecture,
            signature: "unknown".into(),
            ..Default::default()
        };
        package.digest = sha256_with_progress(&resolved_path, progress)?;
        match kind.as_str() {
            "deb" => self.inspect_deb(&mut package)?,
            "rpm" => self.inspect_rpm(&mut package)?,
            "arch" => self.inspect_arch(&mut package)?,
            "msi" => self.inspect_msi(&mut package)?,
            "exe" | "setup" => self.inspect_pe_icon(&mut package)?,
            "appimage" => self.inspect_appimage(&mut package)?,
            "tar" => self.inspect_archive(&mut package)?,
            "binary" => self.inspect_binary(&mut package)?,
            #[cfg(feature = "gui")]
            "flatpak" => self.inspect_flatpak_bundle(&mut package)?,
            "flatpakref" => inspect_flatpak_ref(&mut package)?,
            _ => {}
        }
        self.verify_integrity(&mut package)?;
        if ["deb", "rpm", "arch"].contains(&kind.as_str())
            && (!valid_package_identifier(&package.identifier)
                || package.version.is_empty()
                || package.architecture.is_empty())
        {
            return Err(PackliftError::Message(
                "The package is missing required name, version, or architecture metadata.".into(),
            ));
        }
        let (plan, _) = self.package_plan(&package)?;
        Ok((package, plan))
    }

    fn verify_integrity(&self, package: &mut Package) -> Result<()> {
        let sidecar = PathBuf::from(format!("{}.sha256", package.path.display()));
        if sidecar.is_file() {
            let contents = fs::read_to_string(sidecar)?;
            let expected = contents.split_whitespace().next().unwrap_or("");
            if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(PackliftError::Message(
                    "The SHA-256 sidecar is invalid.".into(),
                ));
            }
            package.checksum = expected.to_ascii_lowercase();
            if package.checksum != package.digest {
                return Err(PackliftError::Message(
                    "The package SHA-256 does not match its .sha256 file.".into(),
                ));
            }
        }
        if package.kind == "rpm" {
            if let Ok(output) = self.runner.capture(
                &[
                    "rpmkeys".into(),
                    "--checksig".into(),
                    "--verbose".into(),
                    package.path.display().to_string(),
                ],
                false,
            ) {
                if Self::rpm_signature_verified(&output) {
                    package.signature = "verified".into();
                }
            }
        }
        let signature = ["sig", "asc"]
            .into_iter()
            .map(|extension| PathBuf::from(format!("{}.{}", package.path.display(), extension)))
            .find(|path| path.is_file());
        let keyring = self.data.join("trustedkeys.gpg");
        if let Some(signature) = signature.filter(|_| keyring.is_file()) {
            self.runner.capture(
                &[
                    "gpgv".into(),
                    "--keyring".into(),
                    keyring.display().to_string(),
                    signature.display().to_string(),
                    package.path.display().to_string(),
                ],
                !host_access_available(),
            )?;
            package.signature = "verified".into();
        }
        Ok(())
    }

    fn rpm_signature_verified(output: &str) -> bool {
        output.lines().any(|line| {
            let lower = line.to_ascii_lowercase();
            let status = lower
                .rsplit([':', ' '])
                .find(|token| !token.is_empty())
                .unwrap_or("")
                .trim_matches(['.', ',', ';']);
            lower.contains("signature")
                && matches!(status, "ok" | "good")
                && !lower.contains("nokey")
                && !lower.contains("not trusted")
        })
    }

    fn inspect_deb(&self, package: &mut Package) -> Result<()> {
        let output = if self.runner.available("dpkg-deb") && !self.runner.sandbox {
            self.runner.capture(
                &[
                    "dpkg-deb".into(),
                    "--field".into(),
                    package.path.to_string_lossy().into_owned(),
                ],
                true,
            )?
        } else {
            let listing = self.runner.capture(
                &[
                    "bsdtar".into(),
                    "-tf".into(),
                    package.path.display().to_string(),
                ],
                true,
            )?;
            let member = listing
                .lines()
                .find(|name| {
                    name.starts_with("control.tar")
                        && name
                            .strip_prefix("control.tar")
                            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('.'))
                })
                .ok_or_else(|| {
                    PackliftError::Message(
                        "The Debian package is missing its control archive.".into(),
                    )
                })?;
            let script = if member.ends_with(".zst") {
                format!(
                    "bsdtar -xOf {} {} | zstd -dc | bsdtar -xOf - ./control control",
                    crate::system::shell_quote(&package.path.display().to_string()),
                    crate::system::shell_quote(member)
                )
            } else {
                format!(
                    "bsdtar -xOf {} {} | bsdtar -xOf - ./control control",
                    crate::system::shell_quote(&package.path.display().to_string()),
                    crate::system::shell_quote(member)
                )
            };
            self.runner
                .capture(&["/bin/sh".into(), "-c".into(), script], true)?
        };
        let fields = crate::core::control_fields(&output, ':');
        package.name = fields
            .get("Package")
            .cloned()
            .unwrap_or(package.name.clone());
        package.identifier = package.name.clone();
        package.version = fields.get("Version").cloned().unwrap_or_default();
        package.architecture = fields.get("Architecture").cloned().unwrap_or_default();
        package.description = fields.get("Description").cloned().unwrap_or_default();
        package.requires = ["Pre-Depends", "Depends"]
            .into_iter()
            .filter_map(|key| fields.get(key))
            .flat_map(|deps| deps.split(',').map(|value| value.trim().to_owned()))
            .filter(|value| !value.is_empty())
            .collect();
        Ok(())
    }

    fn inspect_rpm(&self, package: &mut Package) -> Result<()> {
        let mut file = fs::File::open(&package.path)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(96))?;
        read_rpm_header(&mut file)?;
        let offset = file.stream_position()?;
        file.seek(SeekFrom::Start(offset + ((8 - offset % 8) % 8)))?;
        let (tags, data) = read_rpm_header(&mut file)?;
        package.name = rpm_string(&tags, &data, 1000).unwrap_or_else(|| package.name.clone());
        package.identifier = package.name.clone();
        let version = rpm_string(&tags, &data, 1001).unwrap_or_default();
        let release = rpm_string(&tags, &data, 1002).unwrap_or_default();
        let distro = rpm_string(&tags, &data, 1010)
            .unwrap_or_default()
            .to_lowercase();
        package.rpm_family = if distro.contains("suse") {
            "opensuse"
        } else if distro.contains("fedora") || release.contains(".fc") {
            "fedora"
        } else {
            ""
        }
        .into();
        package.version = [version, release]
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        package.architecture = rpm_string(&tags, &data, 1022).unwrap_or_default();
        package.description = rpm_string(&tags, &data, 1004).unwrap_or_default();
        package.requires = rpm_strings(&tags, &data, 1049);
        Ok(())
    }

    fn inspect_arch(&self, package: &mut Package) -> Result<()> {
        let output = self.runner.capture(
            &[
                "bsdtar".into(),
                "-xOf".into(),
                package.path.display().to_string(),
                ".PKGINFO".into(),
            ],
            true,
        )?;
        let fields = crate::core::control_fields(&output, '=');
        package.name = fields
            .get("pkgname")
            .cloned()
            .unwrap_or(package.name.clone());
        package.identifier = package.name.clone();
        package.version = fields.get("pkgver").cloned().unwrap_or_default();
        package.architecture = fields.get("arch").cloned().unwrap_or_default();
        package.requires = output
            .lines()
            .filter_map(|line| line.strip_prefix("depend = ").map(str::to_owned))
            .collect();
        Ok(())
    }

    fn inspect_msi(&self, package: &mut Package) -> Result<()> {
        if let Ok(output) = self.runner.capture(
            &[
                "msiinfo".into(),
                "export".into(),
                package.path.to_string_lossy().into_owned(),
                "Property".into(),
            ],
            true,
        ) {
            let properties = output
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .collect::<std::collections::HashMap<_, _>>();
            package.name = properties
                .get("ProductName")
                .copied()
                .unwrap_or(&package.name)
                .into();
            package.version = properties
                .get("ProductVersion")
                .copied()
                .unwrap_or_default()
                .into();
            package.identifier = properties
                .get("ProductCode")
                .copied()
                .unwrap_or_default()
                .into();
        }
        if let Ok(output) = self.runner.capture(
            &[
                "msiinfo".into(),
                "suminfo".into(),
                package.path.display().to_string(),
            ],
            true,
        ) {
            if let Some(value) = output.lines().find_map(|line| {
                line.strip_prefix("Template:")
                    .or_else(|| line.strip_prefix("Architecture:"))
            }) {
                package.architecture = match value.trim().split(';').next().unwrap_or("") {
                    "Intel" => "win32".into(),
                    "x64" => "win64".into(),
                    other => other.into(),
                };
            }
        }
        Ok(())
    }

    fn inspect_archive(&self, package: &mut Package) -> Result<()> {
        let output = self.runner.capture(
            &[
                "bsdtar".into(),
                "-tf".into(),
                package.path.to_string_lossy().into_owned(),
            ],
            true,
        )?;
        let executable = output
            .lines()
            .find(|line| line.to_ascii_lowercase().ends_with(".exe"));
        if let Some(path) = executable {
            package.name = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&package.name)
                .to_owned();
        } else if let Some(name) = package.path.file_name().and_then(|s| s.to_str()) {
            package.name = name
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tar.xz")
                .trim_end_matches(".tar.lzma")
                .trim_end_matches(".tar")
                .to_owned();
        }
        Ok(())
    }

    fn inspect_binary(&self, package: &mut Package) -> Result<()> {
        let parent = package.path.parent().unwrap_or(&package.path);
        let bin_stem = package
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Program");
        let mut chars = bin_stem.chars();
        package.name = match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => "Program".into(),
        };
        package.identifier = package.name.to_ascii_lowercase().replace(' ', "-");
        package.desktop_id = format!("{}.desktop", package.identifier);

        for version_file in ["version", "VERSION", "version.txt", "VERSION.txt"] {
            let vp = parent.join(version_file);
            if vp.is_file() {
                if let Ok(content) = fs::read_to_string(&vp) {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() && trimmed.len() <= 32 {
                        package.version = trimmed.to_owned();
                        break;
                    }
                }
            }
        }

        let candidates = [
            parent.join(format!("{}.png", bin_stem)),
            parent.join(format!("{}.svg", bin_stem)),
            parent.join("icon.png"),
            parent.join("icon.svg"),
            parent.join("logo.png"),
            parent.join("resources/app/icon.png"),
            parent.join("resources/icon.png"),
        ];
        for candidate in candidates {
            if candidate.is_file() {
                package.icon = Some(candidate);
                break;
            }
        }
        Ok(())
    }

    fn inspect_appimage(&self, package: &mut Package) -> Result<()> {
        let Some(offset) = appimage_offset(&package.path)? else {
            return Ok(());
        };
        let source = package.path.display().to_string();
        let listing = self.runner.capture(
            &[
                "unsquashfs".into(),
                "-o".into(),
                offset.to_string(),
                "-ls".into(),
                source.clone(),
                "*.desktop".into(),
                ".DirIcon".into(),
                "*.png".into(),
                "*.svg".into(),
                "*.xpm".into(),
            ],
            true,
        )?;
        let files = listing
            .lines()
            .filter_map(|line| line.strip_prefix("squashfs-root/"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let requested_icon = self.read_appimage_desktop(package, offset, &source, &files);
        let icon_name = Path::new(&requested_icon)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut candidates = files
            .into_iter()
            .filter(|name| {
                ["png", "svg", "xpm"].iter().any(|suffix| {
                    Path::new(name)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case(suffix))
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|name| {
            let basename = Path::new(name)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            (
                basename != icon_name
                    && Path::new(name)
                        .file_stem()
                        .and_then(|v| v.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        != icon_name,
                name.contains('/'),
                name.clone(),
            )
        });
        candidates.push(".DirIcon".into());
        for candidate in candidates.into_iter().take(16) {
            let Ok(bytes) = self.runner.capture_bytes(
                &[
                    "unsquashfs".into(),
                    "-o".into(),
                    offset.to_string(),
                    "-no-wildcards".into(),
                    "-cat".into(),
                    source.clone(),
                    candidate.clone(),
                ],
                true,
            ) else {
                continue;
            };
            let suffix = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png"
            } else if bytes.starts_with(b"/* XPM */") {
                "xpm"
            } else if bytes.starts_with(b"<svg")
                || String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]).contains("<svg")
            {
                "svg"
            } else {
                continue;
            };
            let directory = self.data.join("package-icons");
            fs::create_dir_all(&directory)?;
            let icon = directory.join(format!("{}.{}", package.digest, suffix));
            fs::write(&icon, bytes)?;
            package.icon = Some(icon);
            break;
        }
        Ok(())
    }

    fn inspect_appimage_desktop(&self, package: &mut Package) -> Result<()> {
        let Some(offset) = appimage_offset(&package.path)? else {
            return Ok(());
        };
        let source = package.path.display().to_string();
        let listing = self.runner.capture(
            &[
                "unsquashfs".into(),
                "-o".into(),
                offset.to_string(),
                "-ls".into(),
                source.clone(),
                "*.desktop".into(),
            ],
            true,
        )?;
        let files = listing
            .lines()
            .filter_map(|line| line.strip_prefix("squashfs-root/"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        self.read_appimage_desktop(package, offset, &source, &files);
        Ok(())
    }

    fn read_appimage_desktop(
        &self,
        package: &mut Package,
        offset: u64,
        source: &str,
        files: &[String],
    ) -> String {
        let mut desktop_files = files
            .iter()
            .filter(|name| {
                name.ends_with(".desktop")
                    && (!name.contains('/') || name.starts_with("usr/share/applications/"))
            })
            .cloned()
            .collect::<Vec<_>>();
        desktop_files.sort_by_key(|name| (name.contains('/'), name.clone()));
        let mut requested_icon = String::new();
        for desktop in desktop_files.iter().take(8) {
            let output = self.runner.capture(
                &[
                    "unsquashfs".into(),
                    "-o".into(),
                    offset.to_string(),
                    "-no-wildcards".into(),
                    "-cat".into(),
                    source.into(),
                    desktop.clone(),
                ],
                true,
            );
            let Ok(output) = output else { continue };
            let mut in_entry = false;
            let mut has_metadata = false;
            for line in output.lines() {
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry {
                    continue;
                }
                if let Some(value) = line.strip_prefix("Name=") {
                    if !value.trim().is_empty() {
                        package.name = value.trim().to_owned();
                        has_metadata = true;
                    }
                } else if let Some(value) = line.strip_prefix("Icon=") {
                    requested_icon = value.trim().to_owned();
                } else if let Some(value) = line.strip_prefix("StartupWMClass=") {
                    package.startup_wm_class = value.trim().to_owned();
                } else if line == "Terminal=true" {
                    package.terminal = true;
                }
            }
            if has_metadata || !requested_icon.is_empty() || !package.startup_wm_class.is_empty() {
                package.desktop_id = Path::new(desktop)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned();
                break;
            }
        }
        requested_icon
    }

    fn inspect_pe_icon(&self, package: &mut Package) -> Result<()> {
        if let Some(bytes) = pe_icon(&package.path)? {
            let directory = self.data.join("package-icons");
            fs::create_dir_all(&directory)?;
            let converted = ico_to_png(&bytes);
            let (suffix, bytes) = converted
                .map(|png| ("png", png))
                .unwrap_or_else(|| ("ico", bytes));
            let icon = directory.join(format!("{}.{}", package.digest, suffix));
            fs::write(&icon, bytes)?;
            package.icon = Some(icon);
        }
        Ok(())
    }

    pub fn install_package(&self, package: &Package, runtime: &str) -> Result<Value> {
        self.runner
            .write("Checking package compatibility with this system");
        let (plan, machine) = self.package_plan(package)?;
        match plan.method.as_str() {
            "wine" => self.install_windows_program(package, runtime, &machine),
            "flatpak" => self.install_flatpak(package),
            "local" => self.install_local(package, &machine),
            "native" | "toolbox" => self.install_system_package(package, &plan, false),
            _ => Err(PackliftError::Message(
                "Unsupported package installation plan.".into(),
            )),
        }
    }

    fn package_plan(&self, package: &Package) -> Result<(crate::core::Plan, String)> {
        if matches!(package.kind.as_str(), "flatpak" | "flatpakref") {
            if !host_access_available() {
                return Err(PackliftError::HostAccessRequired);
            }
            if !self.runner.available("flatpak") {
                return Err(PackliftError::Message(
                    "The system Flatpak command is required to install this package.".into(),
                ));
            }
            let host = crate::core::host_info("", std::env::consts::ARCH, false);
            return Ok((crate::core::plan_for(package, &host)?, host.1));
        }
        let host = if host_access_available() {
            let machine = self.machine()?;
            let release = self
                .runner
                .capture(&["cat".into(), "/etc/os-release".into()], false)
                .unwrap_or_default();
            crate::core::host_info(&release, &machine, self.immutable_host()?)
        } else {
            if !matches!(package.kind.as_str(), "appimage" | "tar" | "binary") {
                return Err(PackliftError::HostAccessRequired);
            }
            crate::core::host_info("", std::env::consts::ARCH, false)
        };
        let plan = crate::core::plan_for(package, &host)?;
        Ok((plan, host.1))
    }

    #[cfg(feature = "gui")]
    fn inspect_flatpak_bundle(&self, package: &mut Package) -> Result<()> {
        let Some((reference, _, metadata)) = read_flatpak_bundle(&package.path) else {
            return Ok(());
        };
        let Some((id, arch, _)) = flatpak_ref_parts(&reference) else {
            return Ok(());
        };
        package.identifier = id.into();
        package.architecture = arch.into();
        package.name = if id == APP_ID {
            "Packlift".into()
        } else {
            id.into()
        };
        if let Some(data) = metadata.lookup_value("appdata", Some(glib::VariantTy::BYTE_STRING)) {
            if let Ok(compressed) = data.fixed_array::<u8>() {
                let mut xml = String::new();
                let decoded = flate2::read::GzDecoder::new(compressed)
                    .take(1024 * 1024 + 1)
                    .read_to_string(&mut xml);
                if decoded.is_ok() && xml.len() <= 1024 * 1024 {
                    if let Ok(document) = roxmltree::Document::parse(&xml) {
                        if let Some(name) = document
                            .descendants()
                            .find(|node| {
                                node.has_tag_name("name")
                                    && node
                                        .parent()
                                        .is_some_and(|parent| parent.has_tag_name("component"))
                                    && node
                                        .attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                                        .is_none()
                            })
                            .and_then(|node| node.text())
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                        {
                            package.name = name.into();
                        }
                    }
                }
            }
        }
        for key in ["icon-128", "icon-64"] {
            let Some(data) = metadata.lookup_value(key, Some(glib::VariantTy::BYTE_STRING)) else {
                continue;
            };
            let Ok(bytes) = data.fixed_array::<u8>() else {
                continue;
            };
            if bytes.len() > 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                continue;
            }
            let valid_size = bytes.get(16..24).is_some_and(|size| {
                size.chunks_exact(4)
                    .all(|value| (1..=512).contains(&u32::from_be_bytes(value.try_into().unwrap())))
            });
            if !valid_size {
                continue;
            }
            let directory = self.data.join("package-icons");
            fs::create_dir_all(&directory)?;
            let icon = directory.join(format!("{}.png", package.digest));
            fs::write(&icon, bytes)?;
            package.icon = Some(icon);
            break;
        }
        Ok(())
    }

    fn install_flatpak(&self, package: &Package) -> Result<Value> {
        let _operation = operation(&self.data)?;
        #[cfg(feature = "gui")]
        let bundle = (package.kind == "flatpak")
            .then(|| read_flatpak_bundle(&package.path))
            .flatten()
            .map(|(reference, commit, _)| (reference, commit));
        #[cfg(not(feature = "gui"))]
        let bundle: Option<(String, String)> = None;
        if let Some((reference, commit)) = &bundle {
            // Some Flatpak versions abbreviate the commit even with active:full.
            if flatpak_ref_parts(reference).is_some_and(|(id, _, _)| id == APP_ID)
                && self
                    .runner
                    .capture(
                        &[
                            "flatpak".into(),
                            "info".into(),
                            "--user".into(),
                            "--show-commit".into(),
                            "--".into(),
                            reference.clone(),
                        ],
                        false,
                    )
                    .is_ok_and(|active| active.trim() == commit)
            {
                self.runner
                    .write("This Packlift build is already installed for the current user");
                return Ok(json!({
                    "name": package.name, "method": "flatpak", "flatpak_app_id": APP_ID,
                    "flatpak_already_installed": true,
                }));
            }
        }
        let before = if bundle.is_none() {
            self.flatpak_apps()?
        } else {
            BTreeMap::new()
        };
        let option = if package.kind == "flatpakref" {
            "--from"
        } else {
            "--bundle"
        };
        self.runner.write("Installing Flatpak for the current user");
        self.runner.run(
            &[
                "flatpak".into(),
                "install".into(),
                "--user".into(),
                "--noninteractive".into(),
                option.into(),
                "--".into(),
                std::path::absolute(&package.path)?
                    .to_string_lossy()
                    .into_owned(),
            ],
            false,
            true,
            true,
        )?;

        let mut record = json!({"name": package.name, "method": "flatpak"});
        if let Some((id, _, _)) = bundle
            .as_ref()
            .and_then(|(reference, _)| flatpak_ref_parts(reference))
        {
            record["flatpak_app_id"] = json!(id);
        }
        let mut changed = self.flatpak_apps()?.into_iter().filter(|(reference, app)| {
            if let Some((expected_ref, _)) = &bundle {
                return reference == expected_ref;
            }
            (package.identifier.is_empty()
                || reference.split('/').next() == Some(package.identifier.as_str()))
                && before.get(reference).map(|installed| &installed.commit) != Some(&app.commit)
        });
        if let (Some((reference, app)), None) = (changed.next(), changed.next()) {
            let mut parts = reference.split('/');
            let id = parts.next().unwrap_or_default();
            let arch = parts.next().unwrap_or_default();
            let branch = parts.next().unwrap_or_default();
            record = json!({
                "id": short_hash(&format!("flatpak:{reference}"))?,
                "name": if app.name.is_empty() { id } else { &app.name },
                "version": app.version, "architecture": arch, "method": "flatpak",
                "flatpak_app_id": id, "flatpak_scope": "user", "target": reference, "source": package.path, "digest": package.digest,
                "exports": [{
                    "icon": id,
                    "argv": flatpak_launch_command("--user", id, arch, branch),
                }],
            });
            self.registry.save(&record)?;
        } else {
            self.runner
                .write("No single installed Flatpak application could be identified");
        }
        Ok(record)
    }

    fn flatpak_apps(&self) -> Result<BTreeMap<String, FlatpakApp>> {
        Ok(self
            .installed_flatpaks(Some("--user"))?
            .into_iter()
            .map(|(_, reference, app)| (reference, app))
            .collect())
    }

    fn installed_flatpaks(
        &self,
        installation: Option<&str>,
    ) -> Result<Vec<(String, String, FlatpakApp)>> {
        let mut args = vec![
            "flatpak".into(),
            "list".into(),
            "--app".into(),
            "--columns=installation,ref,active:full,name,version".into(),
        ];
        if let Some(installation) = installation {
            args.push(installation.into());
        }
        let output = self.runner.capture(&args, false)?;
        Ok(flatpak_apps_from_list(&output))
    }

    pub fn library_apps(&self) -> Result<Vec<Value>> {
        let mut records = self.registry.apps()?;
        if !host_access_available() || !self.runner.available("flatpak") {
            return Ok(records);
        }
        if let Ok(flatpaks) = self.flatpak_records(&records) {
            records.retain(|record| record["method"] != "flatpak");
            records.extend(flatpaks);
        }
        Ok(records)
    }

    pub fn installed_flatpak_ids(&self) -> Result<BTreeSet<String>> {
        let installations = self.installed_flatpaks(None).or_else(|_| {
            let user = self.installed_flatpaks(Some("--user"));
            let system = self.installed_flatpaks(Some("--system"));
            match (user, system) {
                (Ok(mut user), Ok(system)) => {
                    user.extend(system);
                    Ok(user)
                }
                (Ok(apps), Err(_)) | (Err(_), Ok(apps)) => Ok(apps),
                (Err(error), Err(_)) => Err(error),
            }
        })?;
        Ok(installations
            .into_iter()
            .filter_map(|(_, reference, _)| {
                flatpak_ref_parts(&reference).map(|(id, _, _)| id.to_owned())
            })
            .collect())
    }

    pub fn flatpak_app_installations(&self, id: &str) -> Result<Vec<Value>> {
        Ok(self
            .flatpak_records(&self.registry.apps()?)?
            .into_iter()
            .filter(|record| record["flatpak_app_id"].as_str() == Some(id))
            .collect())
    }

    fn flatpak_records(&self, records: &[Value]) -> Result<Vec<Value>> {
        let installed = self
            .installed_flatpaks(None)
            .or_else(|_| self.installed_flatpaks(Some("--user")))?;

        let managed: BTreeSet<String> = records
            .iter()
            .filter(|record| record["method"] == "flatpak")
            .filter_map(|record| record["target"].as_str().map(str::to_owned))
            .collect();
        let mut records = Vec::new();
        for (scope, reference, app) in installed {
            let Some(flag) = crate::flatpak::installation_flag(&scope) else {
                continue;
            };
            let Some((id, arch, branch)) = flatpak_ref_parts(&reference) else {
                continue;
            };
            let external = scope != "user" || !managed.contains(&reference);
            let key = if external {
                format!("flatpak:{scope}:{reference}")
            } else {
                format!("flatpak:{reference}")
            };
            records.push(json!({
                "id": short_hash(&key)?,
                "name": if app.name.is_empty() { id } else { &app.name },
                "version": app.version, "architecture": arch,
                "method": "flatpak", "flatpak_app_id": id, "target": reference, "flatpak_scope": scope,
                "external_flatpak": external,
                "exports": [{
                    "icon": id,
                    "argv": flatpak_launch_command(&flag, id, arch, branch),
                }],
            }));
        }
        Ok(records)
    }

    fn install_system_package(
        &self,
        package: &Package,
        plan: &crate::core::Plan,
        repair: bool,
    ) -> Result<Value> {
        let _operation = operation(&self.data)?;
        self.runner.write(if plan.method == "toolbox" {
            "Preparing the package container environment"
        } else {
            "Preparing native package installation"
        });
        let container = if plan.method == "toolbox" {
            self.ensure_toolbox(&plan.family)?
        } else {
            String::new()
        };
        let target = package_target(package, &plan.manager);
        let identifier = short_hash(&format!("{}:{}", package.kind, package.digest))?;
        let directory = self.data.join("installed").join(&identifier);
        let pending = directory.join("pending-install.json");
        let receipt = serde_json::to_vec(
            &json!({"container":container,"target":target,"version":package.version,"manager":plan.manager}),
        )?;
        let installed = already_installed(&self.runner, &plan.manager, &target, &container)?;
        if installed && !repair && fs::read(&pending).ok().as_ref() != Some(&receipt) {
            return Err(PackliftError::Message(
                "This system package is already installed.".into(),
            ));
        }
        if installed && !repair {
            let query = match plan.manager.as_str() {
                "apt" => vec!["dpkg-query", "-W", "-f=${Version}", &target],
                "dnf" | "zypper" => vec!["rpm", "-q", "--qf", "%{VERSION}-%{RELEASE}", &target],
                _ => vec!["pacman", "-Q", &target],
            };
            let current = package_capture(
                &self.runner,
                &container,
                &query.into_iter().map(String::from).collect::<Vec<_>>(),
            )?;
            if current.split_whitespace().last() != Some(package.version.as_str()) {
                return Err(PackliftError::Message("The installed version changed since the interrupted operation. Review the package before retrying.".into()));
            }
        }
        if !installed || repair {
            let needs_apt_i386 = plan.manager == "apt"
                && (!container.is_empty()
                    || plan.multilib
                    || package
                        .requires
                        .iter()
                        .any(|dependency| dependency.contains(":i386")));
            let added_apt_i386 = if needs_apt_i386 {
                enable_apt_i386(&self.runner, &container)?
            } else {
                false
            };
            if !container.is_empty() && plan.manager == "pacman" {
                elevated(
                    &self.runner,
                    &container,
                    &[
                        "sh".into(),
                        "-ec".into(),
                        r#"repos=$(pacman-conf --repo-list)
if ! printf '%s\n' "$repos" | grep -Fxq multilib; then
    printf '\n[multilib]\nInclude = /etc/pacman.d/mirrorlist\n' >> /etc/pacman.conf
fi"#
                        .into(),
                    ],
                )?;
            }
            let source = package.path.to_string_lossy().into_owned();
            if !container.is_empty() {
                package_capture(&self.runner, &container, &["test".into(), "-r".into(), source.clone()])
                    .map_err(|_| PackliftError::Message(format!(
                        "The {container} container cannot read the package at {source}. Put it in your home folder and reopen it."
                    )))?;
            }
            self.runner.write("Downloading package dependencies");
            if added_apt_i386
                || !package_indexes_are_fresh(&self.runner, &plan.manager, &container)?
            {
                if container.is_empty() && plan.manager == "pacman" {
                    self.runner.write(
                        "Arch package indexes are old. Packlift will not run a partial pacman upgrade. Update the system with your system updater, then retry.",
                    );
                } else {
                    let refresh: &[&str] = match plan.manager.as_str() {
                        "apt" => &["apt-get", "update", "--error-on=any"],
                        "dnf" => &["dnf", "makecache", "--refresh"],
                        "zypper" => &["zypper", "--non-interactive", "refresh"],
                        "pacman" => &["pacman", "-Syu", "--noconfirm"],
                        _ => {
                            return Err(PackliftError::Message(
                                "Unsupported target package manager.".into(),
                            ))
                        }
                    };
                    elevated(
                        &self.runner,
                        &container,
                        &refresh.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    )?;
                }
            }
            let mut command: Vec<String> = match plan.manager.as_str() {
                "apt" => ["apt-get", "install", "--no-remove", "-y"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                "dnf" => ["dnf", "install", "-y"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                "zypper" => ["zypper", "--non-interactive", "install"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                "pacman" => ["pacman", "-U", "--noconfirm", "--"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                _ => {
                    return Err(PackliftError::Message(
                        "Unsupported package manager.".into(),
                    ))
                }
            };
            if repair && installed {
                match plan.manager.as_str() {
                    "apt" => command.push("--reinstall".into()),
                    "dnf" => command[1] = "reinstall".into(),
                    "zypper" => command.push("--force".into()),
                    _ => {}
                }
            }
            command.push(source);
            if matches!(plan.manager.as_str(), "apt" | "dnf") {
                let mut download = command.clone();
                let flag = if plan.manager == "apt" {
                    "--download-only"
                } else {
                    "--downloadonly"
                };
                download.insert(2, flag.into());
                elevated(&self.runner, &container, &download)?;
            }
            self.runner.write("Installing the system package");
            fs::create_dir_all(&directory)?;
            fs::write(&pending, &receipt)?;
            elevated(&self.runner, &container, &command)?;
        } else {
            self.runner
                .write("Recovering application shortcuts after an interrupted installation");
        }
        let mut launchers =
            system_launchers(&self.runner, package, &plan.manager, &target, &container)?;
        if !container.is_empty() {
            for (index, (_, _, icon, _, _)) in launchers.iter_mut().enumerate() {
                let source_icon = Path::new(icon);
                if !source_icon.is_absolute()
                    || !source_icon.extension().is_some_and(|extension| {
                        ["png", "svg", "xpm", "ico"]
                            .iter()
                            .any(|supported| extension.eq_ignore_ascii_case(supported))
                    })
                {
                    continue;
                }
                let bytes = package_capture_bytes(
                    &self.runner,
                    &container,
                    &["cat".into(), "--".into(), icon.clone()],
                )?;
                if bytes.len() > 2 * 1024 * 1024 {
                    continue;
                }
                let extension = source_icon
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("png");
                let exported = directory.join(format!("icon-{index}.{extension}"));
                fs::write(&exported, bytes)?;
                *icon = exported.to_string_lossy().into_owned();
            }
        }
        self.runner.write("Creating application menu shortcuts");
        let exports = self.write_exports(&identifier, &launchers, "Utility")?;
        let app = json!({"id":identifier,"name":package.name,"version":package.version,"architecture":package.architecture,
            "method":plan.method,"manager":plan.manager,"target":target,"container":container,
            "source":package.path,"digest":package.digest,"directory":directory,"exports":exports});
        self.registry.save(&app)?;
        fs::remove_file(pending)?;
        Ok(app)
    }

    pub fn repair_container_app(&self, app: &Value) -> Result<Value> {
        let path = Path::new(app["source"].as_str().unwrap_or(""));
        let (mut package, mut plan) = self.probe(path)?;
        if app["method"] != "toolbox" || app["digest"].as_str() != Some(package.digest.as_str()) {
            return Err(PackliftError::Message(
                "Repair requires the original unchanged package file.".into(),
            ));
        }
        let container = app["container"].as_str().unwrap_or("");
        let family = container.strip_prefix("flatapk-").unwrap_or("");
        if !self
            .registry
            .containers()?
            .iter()
            .any(|name| name == container)
            || !matches!(
                (package.kind.as_str(), family),
                ("deb", "ubuntu") | ("rpm", "fedora" | "opensuse") | ("arch", "arch")
            )
        {
            return Err(PackliftError::Message(
                "The recorded environment cannot be repaired.".into(),
            ));
        }
        if package.kind == "rpm" {
            package.rpm_family = family.into();
        }
        package.resolve_libraries = true;
        plan.family = family.into();
        plan.method = "toolbox".into();
        plan.manager = app["manager"].as_str().unwrap_or("").into();
        self.install_system_package(&package, &plan, true)
    }

    fn immutable_host(&self) -> Result<bool> {
        Ok(self
            .runner
            .run(
                &["test".into(), "-e".into(), "/run/ostree-booted".into()],
                false,
                true,
                false,
            )?
            .status
            .success()
            || self.runner.available("transactional-update"))
    }

    pub fn container_setup(&self) -> Result<Option<Vec<String>>> {
        let toolbox = self.runner.available("toolbox");
        let podman = self.runner.available("podman");
        if toolbox && podman {
            return Ok(None);
        }
        let release = self
            .runner
            .capture(&["cat".into(), "/etc/os-release".into()], false)?;
        let (ids, _) = crate::core::host_info(&release, "", false);
        let has = |names: &[&str]| names.iter().any(|id| ids.contains(*id));
        let (cmd, tool): (&[&str], &str) = if self.immutable_host()? {
            if self.runner.available("rpm-ostree") {
                (&["rpm-ostree", "install", "--idempotent"], "toolbox")
            } else if self.runner.available("transactional-update") {
                (&["transactional-update", "pkg", "install"], "toolbox")
            } else {
                return Err(PackliftError::Message("Enable Toolbx and Podman with your immutable system's update mechanism, then reopen this package.".into()));
            }
        } else if has(&["debian", "ubuntu"]) {
            (
                &["apt-get", "install", "--no-remove", "-y"],
                "podman-toolbox",
            )
        } else if has(&["fedora", "rhel", "centos"]) {
            (&["dnf", "install", "-y"], "toolbox")
        } else if has(&["suse", "opensuse", "opensuse-tumbleweed", "opensuse-leap"]) {
            (&["zypper", "--non-interactive", "install"], "toolbox")
        } else if has(&["arch"]) {
            (&["pacman", "-S", "--needed", "--noconfirm"], "toolbox")
        } else {
            return Err(PackliftError::Message("Automatic container setup is unavailable for this distribution. Install Toolbx and Podman, then reopen this package.".into()));
        };
        let mut args: Vec<String> = cmd.iter().map(|arg| arg.to_string()).collect();
        if !toolbox {
            args.push(tool.into());
        }
        if !podman {
            args.push("podman".into());
        }
        Ok(Some(args))
    }

    pub fn setup_container_support(&self, approved: &[String]) -> Result<()> {
        let Some(command) = self.container_setup()? else {
            return Ok(());
        };
        if approved != command.as_slice() {
            return Err(PackliftError::Message(
                "Host prerequisites changed. Reopen this package to review setup.".into(),
            ));
        }
        elevated(&self.runner, "", &command)?;
        self.runner.invalidate_available("toolbox");
        self.runner.invalidate_available("podman");
        if self.container_setup()?.is_some() {
            let message = if self.immutable_host()? {
                "Restart your computer to activate container support, then reopen this package."
            } else {
                "Toolbx or Podman is still unavailable. Check the host package manager output and retry."
            };
            return Err(PackliftError::Message(message.into()));
        }
        Ok(())
    }

    fn ensure_toolbox(&self, family: &str) -> Result<String> {
        self.runner
            .write(&format!("Preparing the {family} Toolbox environment"));
        if !self.runner.available("toolbox") || !self.runner.available("podman") {
            return Err(PackliftError::Message(
                "Install Toolbox and Podman on the host before continuing.".into(),
            ));
        }
        self.runner.capture(&["podman".into(),"info".into(),"--format".into(),"{{.Host.Security.Rootless}}".into()], false)
            .and_then(|value| if value.trim() == "true" { Ok(()) } else { Err(PackliftError::Message("Rootless Podman is required.".into())) })
            .map_err(|_| PackliftError::Message("Podman cannot run rootless for this user. Check podman info, user namespace configuration and container storage.".into()))?;
        let name = format!("flatapk-{family}");
        let result = self.runner.run(
            &[
                "podman".into(),
                "container".into(),
                "exists".into(),
                name.clone(),
            ],
            false,
            true,
            false,
        )?;
        if result.status.success() {
            if !self.registry.containers()?.contains(&name) {
                return Err(PackliftError::Message(format!(
                    "Toolbox container {name} already exists outside Packlift."
                )));
            }
            return Ok(name);
        }
        if result.status.code() != Some(1) {
            return Err(PackliftError::Message(format!(
                "Podman could not check container {name}. See the installation log."
            )));
        }
        let image = std::env::var(format!("FLATAPK_{}_IMAGE", family.to_ascii_uppercase()))
            .unwrap_or_else(|_| match family {
                "ubuntu" => "quay.io/toolbx/ubuntu-toolbox:24.04".into(),
                "opensuse" => "registry.opensuse.org/opensuse/toolbox:latest".into(),
                "fedora" => "registry.fedoraproject.org/fedora-toolbox:43".into(),
                _ => "quay.io/toolbx/arch-toolbox:latest".into(),
            });
        self.runner.run(
            &["podman".into(), "pull".into(), image.clone()],
            false,
            true,
            true,
        )?;
        self.runner.run(
            &[
                "toolbox".into(),
                "create".into(),
                "--container".into(),
                name.clone(),
                "--image".into(),
                image,
            ],
            false,
            true,
            true,
        )?;
        self.registry.add_container(&name)?;
        Ok(name)
    }

    fn write_exports(
        &self,
        id: &str,
        launchers: &[(String, Vec<String>, String, bool, String)],
        category: &str,
    ) -> Result<Vec<Value>> {
        let mut exports = Vec::new();
        for (index, (name, argv, icon, terminal, wm_class)) in launchers.iter().enumerate() {
            let path = self
                .applications
                .join(format!("{APP_ID}.{id}.{index}.desktop"));
            let displayed_icon = if icon.is_empty() {
                "application-x-executable"
            } else {
                icon
            };
            export_desktop_with_wm_class(
                &path,
                name,
                argv,
                displayed_icon,
                *terminal,
                category,
                Some(wm_class)
                    .filter(|value| !value.is_empty())
                    .map(String::as_str),
            )?;
            exports.push(json!({"desktop":path,"name":name,"icon":displayed_icon,"argv":argv,"terminal":terminal}));
        }
        Ok(exports)
    }

    fn install_local(&self, package: &Package, machine: &str) -> Result<Value> {
        let _operation = operation(&self.data)?;
        self.runner.write("Installing the local application files");
        let identifier = short_hash(&format!("{}:{}", package.kind, package.digest))?;
        if self
            .registry
            .apps()?
            .iter()
            .any(|app| app["id"] == identifier)
        {
            return Err(PackliftError::Message(
                "This program is already registered in Packlift.".into(),
            ));
        }
        let directory = self.data.join("installed").join(&identifier);
        fs::create_dir_all(&directory)?;
        let sandbox_only = !host_access_available();
        let result =
            self.install_local_payload(package, machine, &identifier, &directory, sandbox_only);
        if result.is_err() {
            let _ = fs::remove_dir_all(&directory);
        }
        result
    }

    fn install_local_payload(
        &self,
        package: &Package,
        machine: &str,
        identifier: &str,
        directory: &Path,
        sandbox_only: bool,
    ) -> Result<Value> {
        let (argv, terminal) = if package.kind == "appimage" {
            let image = directory.join("program.AppImage");
            fs::copy(&package.path, &image)?;
            fs::set_permissions(&image, fs::Permissions::from_mode(0o700))?;
            if let Some(offset) = appimage_offset(&package.path)? {
                let payload = directory.join("payload");
                self.runner.run(
                    &[
                        "unsquashfs".into(),
                        "-no-progress".into(),
                        "-o".into(),
                        offset.to_string(),
                        "-d".into(),
                        payload.to_string_lossy().into_owned(),
                        image.to_string_lossy().into_owned(),
                    ],
                    true,
                    true,
                    true,
                )?;
                let app_run = payload.join("AppRun");
                let resolved = fs::canonicalize(&app_run).map_err(|_| {
                    PackliftError::Message("The AppImage has no usable AppRun entry.".into())
                })?;
                let canonical_payload = fs::canonicalize(&payload)?;
                if !resolved.starts_with(&canonical_payload) || !resolved.is_file() {
                    return Err(PackliftError::Message(
                        "The AppImage AppRun points outside its payload or is not a file.".into(),
                    ));
                }
                (
                    vec![
                        "env".into(),
                        "-C".into(),
                        payload.to_string_lossy().into_owned(),
                        format!("APPDIR={}", payload.display()),
                        format!("ARGV0={}", app_run.display()),
                        app_run.to_string_lossy().into_owned(),
                    ],
                    package.terminal,
                )
            } else if sandbox_only {
                return Err(PackliftError::Message(
                    "Sandbox mode can only open type 2 AppImages.".into(),
                ));
            } else {
                // Type 1 AppImages have no SquashFS payload to extract. Let
                // their embedded runtime use FUSE directly on the host.
                (vec![image.to_string_lossy().into_owned()], package.terminal)
            }
        } else if package.kind == "tar" {
            let payload = directory.join("payload");
            fs::create_dir_all(&payload)?;
            let targets = unpack_archive(&package.path, &payload)?;
            let host_arch = architecture_group(machine);
            let target = select_executable(
                targets.into_iter().filter(|target| {
                    let Ok(mut file) = fs::File::open(target) else {
                        return false;
                    };
                    let mut header = [0; 20];
                    let Ok(count) = std::io::Read::read(&mut file, &mut header) else {
                        return false;
                    };
                    let arch = crate::core::elf_architecture(&header[..count]);
                    arch == host_arch || (host_arch == "x86_64" && arch == "x86")
                }),
                &package.name,
            )
            .ok_or_else(|| {
                PackliftError::Message(
                    "The archive has no executable compatible with this computer.".into(),
                )
            })?;
            (
                vec![
                    "env".into(),
                    "-C".into(),
                    target.parent().unwrap_or(&payload).to_string_lossy().into(),
                    target.to_string_lossy().into_owned(),
                ],
                false,
            )
        } else if package.kind == "binary" {
            let payload = directory.join("payload");
            fs::create_dir_all(&payload)?;
            let target =
                copy_binary_payload(&package.path, &payload, package.include_binary_siblings)?;

            (
                vec![
                    "env".into(),
                    "-C".into(),
                    payload.to_string_lossy().into_owned(),
                    target.to_string_lossy().into_owned(),
                ],
                package.terminal,
            )
        } else {
            return Err(PackliftError::Message(
                "This local package format is not available in the Rust build yet.".into(),
            ));
        };
        let icon = if let Some(ref icon_path) = package.icon {
            let ext = icon_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png");
            let target_icon = directory.join(format!("icon.{}", ext));
            if fs::copy(icon_path, &target_icon).is_ok() {
                target_icon.to_string_lossy().into_owned()
            } else {
                icon_path.to_string_lossy().into_owned()
            }
        } else if package.kind == "binary" {
            package
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("application-x-executable")
                .to_ascii_lowercase()
        } else {
            "application-x-executable".into()
        };
        let export = if sandbox_only {
            self.runner
                .write("Registering the application in the sandbox library");
            None
        } else {
            let path = self
                .applications
                .join(format!("{APP_ID}.{identifier}.0.desktop"));
            if package.kind != "appimage" {
                export_desktop(&path, &package.name, &argv, &icon, terminal, "Utility")?;
            }
            self.runner.write("Creating the application menu shortcut");
            Some(path)
        };
        let mut app = json!({"id":identifier,"name":package.name,"version":package.version,"architecture":package.architecture,
            "method":"local","sandbox_only":sandbox_only,"source":package.path,"digest":package.digest,"directory":directory,
            "exports":[{"desktop":export,"name":package.name,"icon":icon,"argv":argv,"terminal":terminal}]});
        if !sandbox_only && package.kind == "appimage" {
            app["exports"][0]["window_id"] = json!(package.desktop_id);
            app["exports"][0]["startup_wm_class"] = json!(package.startup_wm_class);
            self.sync_app_menu_entry(&mut app)?;
        }
        self.registry.save(&app)?;
        Ok(app)
    }

    fn install_windows_program(
        &self,
        package: &Package,
        runtime: &str,
        machine: &str,
    ) -> Result<Value> {
        if !["exe", "setup", "msi"].contains(&package.kind.as_str()) {
            return Err(PackliftError::Message("Choose a Windows installer.".into()));
        }
        let _operation = operation(&self.data)?;
        self.runner
            .write("Preparing the shared Windows runtime environment");
        let identifier = short_hash(&format!("{}:{}", package.kind, package.digest))?;
        if self
            .registry
            .apps()?
            .iter()
            .any(|app| app["id"] == identifier)
        {
            return Err(PackliftError::Message(
                "This program is already registered in Packlift.".into(),
            ));
        }
        let runtime = self.windows_runtime(runtime, machine)?;
        let wine = Wine::new(
            self.runner.clone(),
            runtime,
            &self.data,
            &self.applications,
            machine,
        );
        wine.prepare()?;
        let cwd = package.path.parent().unwrap_or(Path::new("/"));
        let roots = windows_search_roots(&wine.prefix, &crate::system::home_dir(), cwd);
        let before = windows_executables(&roots).ok();
        let registrations = windows_registrations(&wine.prefix);
        let arguments = if package.kind == "msi" {
            vec![
                "msiexec.exe".into(),
                "/i".into(),
                package.path.to_string_lossy().into_owned(),
            ]
        } else {
            vec![package.path.to_string_lossy().into_owned()]
        };
        let command = wine.command("wine", &arguments, Some(cwd));
        self.runner.write("Running the Windows installer");
        self.runner
            .run(&command, wine.umu && self.runner.sandbox, true, true)?;
        wine.wait()?;
        self.runner
            .write("Looking for the installed Windows executable");
        let target =
            find_installed_executable(package, &wine.prefix, &registrations, &roots, before);
        let Some(target) = target else {
            self.runner
                .write("Could not identify a single installed Windows executable");
            return Err(PackliftError::InstalledExecutableRequired {
                package: Box::new(package.clone()),
                runtime: wine.runtime,
            });
        };
        self.register_windows_target(package, &wine, &identifier, &target)
    }

    pub fn finish_windows_installation(
        &self,
        package: &Package,
        runtime: &str,
        target: &Path,
    ) -> Result<Value> {
        if !host_access_available() {
            return Err(PackliftError::HostAccessRequired);
        }
        let _operation = operation(&self.data)?;
        let identifier = short_hash(&format!("{}:{}", package.kind, package.digest))?;
        if self
            .registry
            .apps()?
            .iter()
            .any(|app| app["id"] == identifier)
        {
            return Err(PackliftError::Message(
                "This program is already registered in Packlift.".into(),
            ));
        }
        let machine = self.machine()?;
        let runtime = self.windows_runtime(runtime, &machine)?;
        let wine = Wine::new(
            self.runner.clone(),
            runtime,
            &self.data,
            &self.applications,
            &machine,
        );
        self.register_windows_target(package, &wine, &identifier, target)
    }

    fn register_windows_target(
        &self,
        package: &Package,
        wine: &Wine,
        identifier: &str,
        target: &Path,
    ) -> Result<Value> {
        let target = target.canonicalize()?;
        if !target.is_file()
            || !is_program_executable(&target)
            || package.path.canonicalize().ok().as_ref() == Some(&target)
        {
            return Err(PackliftError::Message(
                "Choose the installed game's .exe, not its installer or uninstaller.".into(),
            ));
        }
        let (kind, architecture) = detect(&target)?;
        if kind != "exe" {
            return Err(PackliftError::Message(
                "This file is not a Windows program executable.".into(),
            ));
        }
        let name = target
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Windows program");
        let prefix = windows_prefix_drive(&self.data, &wine.runtime);
        let windows_directory = prefix
            .canonicalize()
            .ok()
            .and_then(|prefix| windows_install_directory(&target, &prefix));
        self.runner.write(&format!(
            "Registering installed Windows executable {}",
            target.display()
        ));
        let export = self
            .applications
            .join(format!("{APP_ID}.{identifier}.0.desktop"));
        let argv = wine.command(
            "wine",
            &[target.to_string_lossy().into_owned()],
            Some(target.parent().unwrap_or(&prefix)),
        );
        let mut installed_program = Package {
            path: target.clone(),
            digest: sha256(&target)?,
            ..Package::default()
        };
        self.inspect_pe_icon(&mut installed_program)?;
        let selected_icon = installed_program.icon.as_deref();
        let icon = selected_icon
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "application-x-executable".into());
        let metadata_directory = self.data.join("windows").join(identifier);
        fs::create_dir_all(&metadata_directory)?;
        let mut app = json!({"id":identifier,"name":name,"version":"","architecture":architecture,
            "method":"wine","source":package.path,"digest":package.digest,"directory":metadata_directory,
            "installer":package.path,"runtime":wine.runtime,"windows_directory":windows_directory,"exports":[{"desktop":export,"name":name,
            "icon":icon,"argv":argv,"terminal":false,"wine_args":[target.to_string_lossy()]}]});
        self.sync_app_menu_entry(&mut app)?;
        self.registry.save(&app)?;
        Ok(app)
    }

    pub fn add_game(&self, package: &Package, runtime: &str) -> Result<Value> {
        if package.kind != "exe" || !package.path.is_file() {
            return Err(PackliftError::Message(
                "Choose the Windows executable from the game's folder.".into(),
            ));
        }
        let _operation = operation(&self.data)?;
        self.runner
            .write("Preparing the shared Windows runtime environment");
        let digest = sha256(&package.path)?;
        let identifier =
            sha256_bytes(format!("game:{}", package.path.display()).as_bytes())?[..20].to_owned();
        if self
            .registry
            .apps()?
            .iter()
            .any(|app| app["id"] == identifier)
        {
            return Err(PackliftError::Message(
                "This program is already registered in Packlift.".into(),
            ));
        }
        let machine = self.machine()?;
        let runtime = self.windows_runtime(runtime, &machine)?;
        let wine = Wine::new(
            self.runner.clone(),
            runtime,
            &self.data,
            &self.applications,
            &machine,
        );
        wine.prepare()?;
        let identifier_path = self.data.join("games").join(&identifier);
        fs::create_dir_all(&identifier_path)?;
        let app_dir = package
            .path
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let export_path = self
            .applications
            .join(format!("{APP_ID}.{identifier}.0.desktop"));
        let argv = wine.command(
            "wine",
            &[package.path.to_string_lossy().to_string()],
            Some(&app_dir),
        );
        let icon = package
            .icon
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "application-x-executable".into());
        let mut app = json!({
            "id": identifier, "name": package.name, "version": package.version,
            "architecture": package.architecture, "source": package.path,
            "digest": digest, "directory": identifier_path, "exports": [{
                "desktop": export_path, "name": package.name, "icon": icon,
                "argv": argv, "terminal": false, "wine_args": [package.path.to_string_lossy()]
            }], "method": "wine", "runtime": wine.runtime,
            "game_shortcut": true, "working_directory": app_dir
        });
        self.sync_app_menu_entry(&mut app)?;
        self.registry.save(&app)?;
        Ok(app)
    }

    fn export_menu_icon(
        &self,
        identifier: &str,
        source: Option<&Path>,
    ) -> Result<(String, Option<String>)> {
        let Some(source) = source.filter(|path| path.is_file()) else {
            return Ok(("application-x-executable".into(), None));
        };
        let extension = source
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let (extension, bytes) = if extension == "ico" {
            let input = fs::read(source)?;
            let Some(png) = ico_to_png(&input) else {
                self.runner.write(
                    "The Windows icon could not be converted. Using the application fallback icon.",
                );
                return Ok(("application-x-executable".into(), None));
            };
            ("png", png)
        } else if ["png", "svg", "xpm"].contains(&extension.as_str()) {
            (extension.as_str(), fs::read(source)?)
        } else {
            return Ok(("application-x-executable".into(), None));
        };
        let directory = self
            .applications
            .parent()
            .unwrap_or(&self.applications)
            .join("icons")
            .join("hicolor")
            .join("256x256")
            .join("apps");
        fs::create_dir_all(&directory)?;
        let icon = directory.join(format!("packlift-{identifier}.{extension}"));
        fs::write(&icon, bytes)?;
        self.runner
            .write(&format!("Exported Windows menu icon to {}", icon.display()));
        let path = icon.to_string_lossy().into_owned();
        Ok((path.clone(), Some(path)))
    }

    fn refresh_application_menu_entries(&self) {
        let Ok(_operation) = operation(&self.data) else {
            return;
        };
        let Ok(apps) = self.registry.apps() else {
            return;
        };
        for record in apps {
            if record["method"] != "wine" && !is_host_appimage(&record) {
                continue;
            }
            let mut updated = record.clone();
            let result = self.sync_app_menu_entry(&mut updated).and_then(|()| {
                if updated != record {
                    self.registry.save(&updated)?;
                }
                Ok(())
            });
            if let Err(error) = result {
                self.runner.write(&format!(
                    "Could not repair application shortcut for {}: {error}",
                    record["name"]
                ));
            }
        }
    }

    fn sync_app_menu_entry(&self, app: &mut Value) -> Result<()> {
        let windows = app["method"] == "wine";
        let Some(identifier) = app["id"].as_str().map(str::to_owned) else {
            return Ok(());
        };
        let Some(entry) = app["exports"]
            .as_array_mut()
            .and_then(|entries| entries.first_mut())
        else {
            return Ok(());
        };
        let (Some(desktop), Some(name), Some(arguments)) = (
            entry["desktop"].as_str().map(PathBuf::from),
            entry["name"].as_str().map(str::to_owned),
            entry["argv"].as_array(),
        ) else {
            return Ok(());
        };
        if desktop.parent() != Some(self.applications.as_path()) {
            return Ok(());
        }
        let mut argv = arguments
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if windows {
            argv = windows_desktop_command(&argv);
            let window_id = entry["wine_args"][0]
                .as_str()
                .map(Path::new)
                .map(wine_window_id)
                .unwrap_or_default();
            entry["window_id"] = json!(window_id);
            entry["startup_wm_class"] = json!(window_id);
        } else if entry["window_id"].is_null() {
            let Some(directory) = app["directory"].as_str() else {
                return Ok(());
            };
            let mut package = Package {
                path: Path::new(directory).join("program.AppImage"),
                ..Package::default()
            };
            // Recover window identity from the installed copy, even if the original download is gone.
            self.inspect_appimage_desktop(&mut package)?;
            app["exports"][0]["window_id"] = json!(package.desktop_id);
            app["exports"][0]["startup_wm_class"] = json!(package.startup_wm_class);
        }

        let game = app["game_shortcut"].as_bool() == Some(true);
        let entry = &mut app["exports"][0];
        let source_icon = entry["icon"]
            .as_str()
            .unwrap_or("application-x-executable")
            .to_owned();
        let icon = if windows {
            if let Some(path) = entry["menu_icon_file"]
                .as_str()
                .filter(|path| Path::new(path).is_file())
            {
                path.to_owned()
            } else {
                let (icon, file) =
                    self.export_menu_icon(&identifier, Some(Path::new(&source_icon)))?;
                entry["menu_icon_file"] = json!(file);
                icon
            }
        } else {
            source_icon
        };
        let window_id = entry["window_id"].as_str().unwrap_or_default();
        let class = entry["startup_wm_class"]
            .as_str()
            .filter(|class| !class.is_empty())
            .or_else(|| (!window_id.is_empty()).then_some(window_id));
        let current = fs::read_to_string(&desktop).unwrap_or_default();
        let property = |key| current.lines().find_map(|line| line.strip_prefix(key));
        let exec = desktop_exec(&argv);
        if property("Exec=") != Some(&exec)
            || property("Icon=") != Some(&icon)
            || property("StartupWMClass=") != class
        {
            export_desktop_with_wm_class(
                &desktop,
                &name,
                &argv,
                &icon,
                entry["terminal"].as_bool() == Some(true),
                if game { "Game" } else { "Utility" },
                class,
            )?;
        }
        if valid_window_id(window_id) {
            let alias = self.applications.join(format!("{window_id}.desktop"));
            if alias != desktop {
                match self.sync_window_alias(&alias, &desktop, &identifier) {
                    Ok(true) => entry["window_desktop"] = json!(alias),
                    Ok(false) => {}
                    Err(error) => self.runner.write(&format!(
                        "Could not create window shortcut for {name}: {error}"
                    )),
                }
            }
        }
        Ok(())
    }

    fn sync_window_alias(&self, alias: &Path, desktop: &Path, identifier: &str) -> Result<bool> {
        let owner = format!("X-Packlift-Window-Owner={identifier}");
        let existing = fs::read_to_string(alias).ok();
        if fs::symlink_metadata(alias).is_ok() {
            if !existing
                .as_ref()
                .is_some_and(|text| text.lines().any(|line| line == owner))
            {
                return Ok(false);
            }
        } else {
            let name = alias
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let result = self.runner.run(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    HOST_DESKTOP_LOOKUP.into(),
                    "packlift".into(),
                    name,
                ],
                false,
                true,
                false,
            )?;
            if result.status.success() {
                return Ok(false);
            }
        }
        let primary = fs::read_to_string(desktop)?;
        let mut content = primary
            .lines()
            .filter(|line| !line.starts_with("StartupWMClass="))
            .collect::<Vec<_>>()
            .join("\n");
        content.push_str(&format!("\nNoDisplay=true\n{owner}\n"));
        if let Some(existing) = existing {
            if existing != content {
                let temporary = alias.with_extension("packlift-tmp");
                fs::write(&temporary, content)?;
                fs::rename(temporary, alias)?;
            }
        } else {
            use std::io::Write;
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(alias);
            match file {
                Ok(mut file) => file.write_all(content.as_bytes())?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(false)
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(true)
    }

    pub fn install_windows(&self, package: &Package, runtime: &str) -> Result<Value> {
        let machine = self.machine()?;
        self.install_windows_program(package, runtime, &machine)
    }

    pub fn installed_proton_versions(&self) -> Vec<PathBuf> {
        let data = crate::system::data_home();
        let home = crate::system::home_dir();
        let roots = [
            self.data.join("umu/compatibilitytools"),
            self.data.join("Steam/compatibilitytools.d"),
            data.join("umu/compatibilitytools"),
            data.join("Steam/compatibilitytools.d"),
            data.join("Steam/steamapps/common"),
            home.join(".steam/root/compatibilitytools.d"),
            home.join(".steam/root/steamapps/common"),
            home.join(".var/app/com.valvesoftware.Steam/data/Steam/compatibilitytools.d"),
            home.join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps/common"),
        ];
        let mut versions = std::collections::BTreeSet::new();
        for root in roots {
            if let Ok(entries) = fs::read_dir(&root) {
                versions.extend(
                    entries
                        .flatten()
                        .filter_map(|entry| entry.path().canonicalize().ok())
                        .filter(|path| path.join("proton").is_file()),
                );
            } else if self.runner.sandbox && host_access_available() {
                // Flatpak can hide Steam's private data even with access to home.
                let command = [
                    "sh".into(), "-c".into(),
                    r#"if [ -d "$1" ]; then find -L "$1" -mindepth 2 -maxdepth 2 -type f -name proton -print0; fi"#.into(),
                    "packlift".into(), root.to_string_lossy().into_owned(),
                ];
                if let Ok(output) = self.runner.capture(&command, false) {
                    versions.extend(
                        output
                            .split('\0')
                            .filter_map(|file| Path::new(file).parent())
                            .filter(|path| path.is_absolute())
                            .map(Path::to_owned),
                    );
                }
            }
        }
        versions.into_iter().collect()
    }

    pub fn set_program_runtime(
        &self,
        id: &str,
        runtime: &str,
        proton_path: Option<&Path>,
    ) -> Result<Value> {
        self.update_windows_program(id, runtime, proton_path, None)
    }

    pub fn set_windows_launch_options(
        &self,
        id: &str,
        runtime: &str,
        proton_path: Option<&Path>,
        options: WindowsLaunchOptions,
    ) -> Result<Value> {
        self.update_windows_program(id, runtime, proton_path, Some(options))
    }

    fn update_windows_program(
        &self,
        id: &str,
        runtime: &str,
        proton_path: Option<&Path>,
        options: Option<WindowsLaunchOptions>,
    ) -> Result<Value> {
        if !host_access_available() {
            return Err(PackliftError::HostAccessRequired);
        }
        let _operation = operation(&self.data)?;
        let record = self
            .registry
            .apps()?
            .into_iter()
            .find(|app| app["id"].as_str() == Some(id) && app["method"] == "wine")
            .ok_or_else(|| {
                PackliftError::Message("This Windows program is no longer in the library.".into())
            })?;
        if !matches!(runtime, "wine" | "proton") {
            return Err(PackliftError::Message("Choose Wine or Proton.".into()));
        }
        if let Some(path) = proton_path {
            if runtime != "proton"
                || !self
                    .installed_proton_versions()
                    .iter()
                    .any(|version| version == path)
            {
                return Err(PackliftError::Message("That Proton version is no longer available. Choose another runtime in Settings.".into()));
            }
        }
        let entry = &record["exports"][0];
        let mut arguments = entry["wine_args"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .filter(|items| !items.is_empty())
            .ok_or_else(|| {
                PackliftError::Message("This shortcut has no Windows executable.".into())
            })?;
        let options = options.unwrap_or_else(|| WindowsLaunchOptions {
            working_directory: record["launch_directory"].as_str().map(PathBuf::from),
            arguments: record["launch_arguments"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            environment: String::new(),
            disable_dxvk: false,
            disable_vkd3d: false,
            disable_esync: false,
            disable_fsync: false,
        });
        if let Some(directory) = &options.working_directory {
            let directory_text = directory.to_string_lossy();
            if !directory.is_absolute()
                || directory_text.contains(char::is_control)
                || (!directory.is_dir()
                    && (!self.runner.sandbox
                        || !self
                            .runner
                            .run(
                                &["test".into(), "-d".into(), directory_text.into_owned()],
                                false,
                                true,
                                false,
                            )?
                            .status
                            .success()))
            {
                return Err(PackliftError::Message(
                    "Choose an existing folder for the working directory.".into(),
                ));
            }
        }
        let extra_arguments = shlex::split(&options.arguments)
            .filter(|arguments| {
                !arguments
                    .iter()
                    .any(|argument| argument.contains(char::is_control))
            })
            .ok_or_else(|| {
                PackliftError::Message(
                    "Check the quotes and characters in the launch arguments.".into(),
                )
            })?;
        arguments.extend(extra_arguments);
        let environment = parse_windows_environment(&options.environment)?;
        for (key, value) in [
            ("PROTON_USE_WINED3D", options.disable_dxvk),
            ("PROTON_NO_D3D12", options.disable_vkd3d),
            ("PROTON_NO_ESYNC", options.disable_esync),
            ("PROTON_NO_FSYNC", options.disable_fsync),
        ] {
            if value && environment.contains_key(key) {
                return Err(PackliftError::Message(format!(
                    "Remove {key} from the custom environment variables because Packlift manages it above."
                )));
            }
        }
        let executable = Path::new(&arguments[0]);
        if entry["desktop"].as_str().is_none() {
            return Err(PackliftError::Message(
                "This program has no desktop shortcut.".into(),
            ));
        }
        let name = entry["name"]
            .as_str()
            .or_else(|| record["name"].as_str())
            .unwrap_or("Program");
        let cwd = options
            .working_directory
            .as_deref()
            .or_else(|| record["working_directory"].as_str().map(Path::new))
            .or_else(|| executable.parent());
        let machine = self.machine()?;
        let resolved_runtime = self.windows_runtime(runtime, &machine)?;
        let mut wine = Wine::new(
            self.runner.clone(),
            resolved_runtime,
            &self.data,
            &self.applications,
            &machine,
        );
        wine.proton_path = proton_path.map(Path::to_owned);
        wine.extra_env = environment.clone();
        for (key, value) in [
            ("PROTON_USE_WINED3D", options.disable_dxvk),
            ("PROTON_NO_D3D12", options.disable_vkd3d),
            ("PROTON_NO_ESYNC", options.disable_esync),
            ("PROTON_NO_FSYNC", options.disable_fsync),
        ] {
            if value {
                wine.extra_env.insert(key.into(), "1".into());
            }
        }
        let argv = wine.command("wine", &arguments, cwd);
        let mut updated = record.clone();
        updated["runtime"] = json!(wine.runtime);
        updated["proton_path"] = json!(proton_path);
        updated["launch_directory"] = json!(options.working_directory);
        updated["launch_arguments"] = json!(options.arguments);
        updated["launch_environment"] = json!(environment);
        updated["wine_options"] = json!({
            "disable_dxvk": options.disable_dxvk,
            "disable_vkd3d": options.disable_vkd3d,
            "disable_esync": options.disable_esync,
            "disable_fsync": options.disable_fsync,
        });
        updated["exports"][0]["argv"] = json!(argv);
        if let Err(error) = self
            .sync_app_menu_entry(&mut updated)
            .and_then(|()| self.registry.save(&updated))
        {
            let _ = self.sync_app_menu_entry(&mut record.clone());
            return Err(error);
        }
        self.runner
            .write(&format!("Updated Windows launch settings for {name}"));
        Ok(updated)
    }

    pub fn launch(&self, app: &Value) -> Result<()> {
        let argv = app
            .get("exports")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
            .and_then(|e| e.get("argv"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                PackliftError::Message("No launch command is registered for this program.".into())
            })?;
        let command: Vec<String> = argv
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if app["sandbox_only"].as_bool() == Some(true) {
            self.runner.start_in_sandbox(&command)
        } else if !host_access_available() {
            Err(PackliftError::HostAccessRequired)
        } else if app["method"] == "flatpak" {
            self.runner.start_detached(&command)
        } else {
            self.runner.start(&command)
        }
    }

    fn machine(&self) -> Result<String> {
        if self.runner.sandbox {
            Ok(self
                .runner
                .capture(
                    &[
                        "flatpak-spawn".into(),
                        "--host".into(),
                        "uname".into(),
                        "-m".into(),
                    ],
                    true,
                )?
                .trim()
                .into())
        } else {
            Ok(std::env::consts::ARCH.into())
        }
    }

    fn windows_runtime(&self, runtime: &str, machine: &str) -> Result<String> {
        if runtime == "proton" {
            if architecture_group(machine) != "x86_64" {
                return Err(PackliftError::Message(
                    "Automatic Proton installation currently requires a 64-bit PC.".into(),
                ));
            }
            let bundled = Path::new("/app/bin/umu-run");
            if self.runner.sandbox {
                let temporary = self.data.join("umu-run.tmp");
                fs::copy(bundled, &temporary).map_err(|_| {
                    PackliftError::Message(
                        "UMU Launcher is missing from this Packlift build.".into(),
                    )
                })?;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
                fs::rename(temporary, self.data.join("umu-run"))?;
            } else if !self.runner.available("umu-run") {
                return Err(PackliftError::Message(
                    "Install UMU Launcher on the host before using Proton.".into(),
                ));
            }
            return Ok("proton".into());
        }
        if runtime == "wine" && self.runner.available("wine") {
            return Ok("wine".into());
        }
        if runtime != "wine" && runtime != FLATPAK_WINE {
            let path = Path::new(runtime);
            if path.is_absolute()
                && fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            {
                return Ok(runtime.into());
            }
            return Err(PackliftError::Message(
                "Choose an executable Wine or Proton runtime installed on the host.".into(),
            ));
        }
        if architecture_group(machine) != "x86_64" || !self.runner.available("flatpak") {
            return Err(PackliftError::Message(
                "Install Wine or choose an installed Wine or Proton runtime.".into(),
            ));
        }
        let info = self.runner.run(
            &["flatpak".into(), "info".into(), FLATPAK_WINE.into()],
            false,
            true,
            false,
        )?;
        if !info.status.success() {
            let _ = self.runner.run(
                &[
                    "flatpak".into(),
                    "remote-add".into(),
                    "--user".into(),
                    "--if-not-exists".into(),
                    "flathub".into(),
                    "https://dl.flathub.org/repo/flathub.flatpakrepo".into(),
                ],
                false,
                true,
                true,
            )?;
            self.runner.run(
                &[
                    "flatpak".into(),
                    "install".into(),
                    "--user".into(),
                    "--noninteractive".into(),
                    "flathub".into(),
                    FLATPAK_WINE.into(),
                ],
                false,
                true,
                true,
            )?;
        }
        Ok(FLATPAK_WINE.into())
    }

    pub fn uninstall_game(&self, app: &Value) -> Result<()> {
        let _operation = operation(&self.data)?;
        remove_exports(app, &self.applications, &self.data);
        self.remove_managed_directory(app, "games")?;
        self.remove_unreferenced_icons(app)?;
        self.registry.remove(app)?;
        Ok(())
    }

    pub fn uninstall(&self, app: &Value) -> Result<()> {
        self.uninstall_with_options(app, false)
    }

    pub fn uninstall_removing_data(&self, app: &Value) -> Result<()> {
        if app["method"] != "flatpak" {
            return Err(PackliftError::Message(
                "Data removal is available for Flatpak applications.".into(),
            ));
        }
        self.uninstall_with_options(app, true)
    }

    fn uninstall_with_options(&self, app: &Value, delete_data: bool) -> Result<()> {
        let _operation = operation(&self.data)?;
        self.runner.write("Removing the installed program");
        let method = app["method"].as_str().unwrap_or("");
        if method == "native" || method == "toolbox" {
            let manager = app["manager"].as_str().unwrap_or("");
            let target = app["target"].as_str().unwrap_or("");
            let container = app["container"].as_str().unwrap_or("");
            if already_installed(&self.runner, manager, target, container)? {
                let base: Vec<String> = match manager {
                    "apt" => ["apt-get", "remove", "-y", "--"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    "dnf" => ["dnf", "remove", "-y", "--"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    "zypper" => ["zypper", "--non-interactive", "remove", "--"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    "pacman" => ["pacman", "-R", "--noconfirm", "--"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    _ => {
                        return Err(PackliftError::Message(
                            "Unsupported package manager.".into(),
                        ))
                    }
                };
                let mut command = base;
                command.push(target.into());
                elevated(&self.runner, container, &command)?;
            }
        } else if method == "flatpak" {
            if !host_access_available() {
                return Err(PackliftError::HostAccessRequired);
            }
            let target = app["target"]
                .as_str()
                .filter(|value| valid_flatpak_ref(value))
                .ok_or_else(|| {
                    PackliftError::Message(
                        "The Flatpak record has no valid application reference.".into(),
                    )
                })?;
            let scope = app["flatpak_scope"].as_str().unwrap_or("user");
            let installation = crate::flatpak::installation_flag(scope)
                .ok_or_else(|| PackliftError::Message("Unknown Flatpak installation.".into()))?;
            if self.installed_flatpaks(Some(&installation))?.iter().any(
                |(installed_scope, reference, _)| installed_scope == scope && reference == target,
            ) {
                let mut command = vec![
                    "flatpak".into(),
                    "uninstall".into(),
                    installation,
                    "--assumeyes".into(),
                ];
                if delete_data {
                    command.push("--delete-data".into());
                }
                command.extend(["--".into(), format!("app/{target}")]);
                self.runner.run(&command, false, true, true)?;
            } else if delete_data {
                return Err(PackliftError::Message(
                    "The selected Flatpak installation was no longer found.".into(),
                ));
            }
        } else if method == "local" {
            self.remove_managed_directory(app, "installed")?;
        } else if method == "wine" && !app["game_shortcut"].as_bool().unwrap_or(false) {
            if let Some(target) = app["windows_directory"].as_str().map(PathBuf::from) {
                let prefix = self.data.join("wine/prefix/drive_c");
                let permitted = target.starts_with(prefix.join("Program Files"))
                    || target.starts_with(prefix.join("Program Files (x86)"));
                let claimed_elsewhere = self.registry.apps()?.iter().any(|other| {
                    other["id"] != app["id"]
                        && other["windows_directory"]
                            .as_str()
                            .is_some_and(|path| Path::new(path) == target)
                });
                if permitted && !claimed_elsewhere && target.is_dir() {
                    fs::remove_dir_all(&target)?;
                }
            }
            self.remove_managed_directory(app, "windows")?;
        }
        remove_exports(app, &self.applications, &self.data);
        self.remove_unreferenced_icons(app)?;
        self.registry.remove(app)?;
        Ok(())
    }

    pub fn clean_unused_environments(&self) -> Result<Vec<String>> {
        let _operation = operation(&self.data)?;
        let used = self
            .registry
            .apps()?
            .iter()
            .filter_map(|app| app["container"].as_str())
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>();
        let mut removed = Vec::new();
        for name in self.registry.containers()? {
            if used.contains(&name) {
                continue;
            }
            let exists = self.runner.run(
                &[
                    "podman".into(),
                    "container".into(),
                    "exists".into(),
                    name.clone(),
                ],
                false,
                true,
                false,
            )?;
            match exists.status.code() {
                Some(0) => {
                    self.runner.run(
                        &[
                            "toolbox".into(),
                            "rm".into(),
                            "--force".into(),
                            name.clone(),
                        ],
                        false,
                        true,
                        true,
                    )?;
                    self.runner
                        .write(&format!("Removed unused Toolbox environment {name}"));
                }
                Some(1) => {
                    self.runner
                        .write(&format!("Removed stale environment record {name}"));
                }
                _ => {
                    return Err(PackliftError::Message(
                        "Podman could not check the unused Toolbox environment.".into(),
                    ));
                }
            }
            self.registry.remove_container(&name)?;
            removed.push(name);
        }
        Ok(removed)
    }

    fn remove_managed_directory(&self, app: &Value, expected_parent: &str) -> Result<()> {
        let Some(directory) = app["directory"].as_str() else {
            return Ok(());
        };
        let directory = PathBuf::from(directory);
        let expected = self.data.join(expected_parent);
        let owned_child = directory.parent() == Some(expected.as_path());
        let safe_parent = expected.canonicalize().is_ok_and(|parent| {
            directory.parent().and_then(|path| path.canonicalize().ok()) == Some(parent)
        });
        let is_real_directory =
            fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_dir());
        if owned_child && safe_parent && is_real_directory {
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }

    fn remove_unreferenced_icons(&self, app: &Value) -> Result<()> {
        let others = self
            .registry
            .apps()?
            .into_iter()
            .filter(|other| other["id"] != app["id"])
            .collect::<Vec<_>>();
        let Some(exports) = app["exports"].as_array() else {
            return Ok(());
        };
        for icon in exports.iter().filter_map(|export| export["icon"].as_str()) {
            let path = Path::new(icon);
            if !path.starts_with(self.data.join("package-icons")) {
                continue;
            }
            let referenced = others.iter().any(|other| {
                other["exports"].as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|export| export["icon"].as_str() == Some(icon))
                })
            });
            if !referenced {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "gui")]
fn read_flatpak_bundle(path: &Path) -> Option<(String, String, glib::VariantDict)> {
    use std::io::{Seek, SeekFrom};

    // The first child of an OSTree static-delta superblock is an a{sv}
    // metadata dictionary. The trailing GVariant tuple offsets are stored
    // in reverse order, so we can read it without loading the delta payload.
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let width = match size {
        0..=255 => 1,
        256..=65535 => 2,
        65536..=4294967295 => 4,
        _ => 8,
    };
    let framing = size.checked_sub(width * 3)?;
    file.seek(SeekFrom::Start(framing)).ok()?;
    let mut offset = || {
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes[..width as usize]).ok()?;
        Some(u64::from_le_bytes(bytes))
    };
    let to = offset()?;
    let from = offset()?;
    let metadata_end = offset()?;
    if metadata_end == 0
        || metadata_end > 8 * 1024 * 1024
        || from < metadata_end.next_multiple_of(8) + 8
        || to.checked_sub(from)? != 32
        || to > framing
    {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = vec![0; metadata_end as usize];
    file.read_exact(&mut bytes).ok()?;
    let metadata = glib::Variant::from_data_with_type(bytes, glib::VariantTy::VARDICT);
    if !metadata.is_normal_form() {
        return None;
    }
    let dict = glib::VariantDict::new(Some(&metadata));
    let reference = dict.lookup::<String>("ref").ok()??;
    let reference = reference.strip_prefix("app/")?;
    flatpak_ref_parts(reference)?;
    let mut bytes = [0; 32];
    file.seek(SeekFrom::Start(from)).ok()?;
    file.read_exact(&mut bytes).ok()?;
    let commit = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some((reference.into(), commit, dict))
}

fn inspect_flatpak_ref(package: &mut Package) -> Result<()> {
    const MAX_SIZE: u64 = 1024 * 1024;
    let mut text = String::new();
    fs::File::open(&package.path)?
        .take(MAX_SIZE + 1)
        .read_to_string(&mut text)?;
    if text.len() > MAX_SIZE as usize {
        return Err(PackliftError::Message(
            "The Flatpak reference is too large.".into(),
        ));
    }
    let fields = text
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[Flatpak Ref]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect::<BTreeMap<_, _>>();
    let name = fields
        .get("Name")
        .filter(|name| valid_flatpak_component(name))
        .ok_or_else(|| {
            PackliftError::Message("The Flatpak reference has no valid application ID.".into())
        })?;
    package.identifier = (*name).into();
    package.name = fields
        .get("Title")
        .filter(|title| !title.is_empty())
        .copied()
        .unwrap_or(name)
        .into();
    package.description = fields.get("Url").copied().unwrap_or_default().into();
    Ok(())
}

struct FlatpakApp {
    commit: String,
    name: String,
    version: String,
}

fn valid_flatpak_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn flatpak_ref_parts(reference: &str) -> Option<(&str, &str, &str)> {
    let mut parts = reference.split('/');
    let mut next = || parts.next().filter(|part| valid_flatpak_component(part));
    let reference = (next()?, next()?, next()?);
    parts.next().is_none().then_some(reference)
}

fn flatpak_launch_command(scope: &str, id: &str, arch: &str, branch: &str) -> [String; 7] {
    [
        "flatpak".into(),
        "run".into(),
        scope.into(),
        format!("--arch={arch}"),
        format!("--branch={branch}"),
        "--".into(),
        id.into(),
    ]
}

fn valid_flatpak_ref(reference: &str) -> bool {
    flatpak_ref_parts(reference).is_some()
}

fn flatpak_apps_from_list(output: &str) -> Vec<(String, String, FlatpakApp)> {
    output
        .lines()
        .filter_map(|line| {
            let mut columns = line.splitn(5, '\t');
            let scope = columns.next()?;
            let reference = columns
                .next()
                .filter(|reference| valid_flatpak_ref(reference))?;
            let commit = columns.next()?;
            Some((
                scope.into(),
                reference.into(),
                FlatpakApp {
                    commit: commit.into(),
                    name: columns.next().unwrap_or_default().into(),
                    version: columns.next().unwrap_or_default().into(),
                },
            ))
        })
        .collect()
}

fn windows_install_directory(executable: &Path, prefix: &Path) -> Option<PathBuf> {
    for root in [
        prefix.join("Program Files"),
        prefix.join("Program Files (x86)"),
    ] {
        if let Ok(relative) = executable.strip_prefix(&root) {
            let first = relative.components().next()?;
            return Some(root.join(first));
        }
    }
    None
}

fn sha256_bytes(value: &[u8]) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(value)))
}

fn read_rpm_header(file: &mut fs::File) -> Result<(RpmHeaderIndex, Vec<u8>)> {
    use std::io::Read;
    let mut intro = [0_u8; 16];
    file.read_exact(&mut intro)
        .map_err(|_| PackliftError::Message("The RPM package metadata is damaged.".into()))?;
    if intro[..4] != [0x8e, 0xad, 0xe8, 0x01] {
        return Err(PackliftError::Message(
            "The RPM package metadata is damaged.".into(),
        ));
    }
    let count = u32::from_be_bytes(intro[8..12].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(intro[12..16].try_into().unwrap()) as usize;
    if count > 65_536 || size > 32 * 1024 * 1024 {
        return Err(PackliftError::Message(
            "The RPM metadata is too large to inspect safely.".into(),
        ));
    }
    let mut index = vec![0_u8; count * 16];
    let mut data = vec![0_u8; size];
    file.read_exact(&mut index)?;
    file.read_exact(&mut data)?;
    let tags = index
        .chunks_exact(16)
        .map(|entry| {
            (
                u32::from_be_bytes(entry[0..4].try_into().unwrap()),
                (
                    u32::from_be_bytes(entry[4..8].try_into().unwrap()),
                    u32::from_be_bytes(entry[8..12].try_into().unwrap()),
                    u32::from_be_bytes(entry[12..16].try_into().unwrap()),
                ),
            )
        })
        .collect();
    Ok((tags, data))
}

fn rpm_strings(
    tags: &std::collections::HashMap<u32, (u32, u32, u32)>,
    data: &[u8],
    tag: u32,
) -> Vec<String> {
    let Some((kind, offset, length)) = tags.get(&tag).copied() else {
        return Vec::new();
    };
    if !matches!(kind, 6 | 8 | 9) || offset as usize >= data.len() || length > 65_536 {
        return Vec::new();
    }
    let values = data[offset as usize..]
        .split(|byte| *byte == 0)
        .take(if kind == 6 { 1 } else { length as usize })
        .filter(|value| !value.is_empty())
        .map(|value| String::from_utf8_lossy(value).into_owned())
        .collect();
    values
}

fn rpm_string(
    tags: &std::collections::HashMap<u32, (u32, u32, u32)>,
    data: &[u8],
    tag: u32,
) -> Option<String> {
    rpm_strings(tags, data, tag).into_iter().next()
}

// A desktop shortcut runs on the host. Reuse the working directory and
// environment from the UMU command without requiring flatpak-spawn there.
fn windows_desktop_command(argv: &[String]) -> Vec<String> {
    let [spawn, host, directory, tail @ ..] = argv else {
        return argv.to_vec();
    };
    if spawn != "flatpak-spawn" || host != "--host" {
        return argv.to_vec();
    }
    let Some(directory) = directory.strip_prefix("--directory=") else {
        return argv.to_vec();
    };
    let env_count = tail
        .iter()
        .take_while(|arg| arg.starts_with("--env="))
        .count();
    let (environment, command) = tail.split_at(env_count);
    if command.is_empty() {
        return argv.to_vec();
    }

    let mut result = vec!["/usr/bin/env".into(), "-C".into(), directory.into()];
    result.extend(
        environment
            .iter()
            .map(|arg| arg["--env=".len()..].to_owned()),
    );
    result.extend_from_slice(command);
    result
}

fn is_host_appimage(record: &Value) -> bool {
    record["method"] == "local"
        && record["sandbox_only"] != true
        && (!record["exports"][0]["window_id"].is_null()
            || record["directory"]
                .as_str()
                .is_some_and(|directory| Path::new(directory).join("program.AppImage").is_file()))
}

fn wine_window_id(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

fn valid_window_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && !value.starts_with('.')
        && !value.contains(['/', '\\'])
        && !value.contains(char::is_control)
}

const HOST_DESKTOP_LOOKUP: &str = r#"
set -f
IFS=:
for directory in ${XDG_DATA_DIRS:-/usr/local/share:/usr/share} /var/lib/flatpak/exports/share "${XDG_DATA_HOME:-$HOME/.local/share}/flatpak/exports/share"; do
    test -e "$directory/applications/$1" && exit 0
done
exit 1
"#;

fn shared_windows_prefix(data: &Path) -> PathBuf {
    data.join("wine/prefix")
}

/// The C: drive whose containing folder is the prefix used by this runtime.
pub fn windows_prefix_drive(data: &Path, runtime: &str) -> PathBuf {
    let prefix = shared_windows_prefix(data);
    if runtime == "proton" {
        let proton_drive = prefix.join("pfx/drive_c");
        if proton_drive.is_dir() {
            return proton_drive;
        }
    }
    prefix.join("drive_c")
}

struct Wine {
    runner: Runner,
    runtime: String,
    prefix: PathBuf,
    integrations: PathBuf,
    umu: bool,
    flatpak: bool,
    proton_path: Option<PathBuf>,
    extra_env: BTreeMap<String, String>,
}

impl Wine {
    fn new(
        runner: Runner,
        runtime: String,
        data: &Path,
        _applications: &Path,
        _machine: &str,
    ) -> Self {
        Self {
            runner,
            umu: runtime == "proton",
            flatpak: runtime == FLATPAK_WINE,
            runtime,
            prefix: shared_windows_prefix(data),
            integrations: data.join("wine/desktop"),
            proton_path: None,
            extra_env: BTreeMap::new(),
        }
    }

    fn command(&self, tool: &str, args: &[String], cwd: Option<&Path>) -> Vec<String> {
        let mut values = vec![("WINEPREFIX", self.prefix.to_string_lossy().to_string())];
        if self.umu {
            values.push(("GAMEID", "umu-default".into()));
            if let Some(path) = &self.proton_path {
                values.push(("PROTONPATH", path.to_string_lossy().into_owned()));
            }
            values.push((
                "UMU_FOLDERS_PATH",
                self.runner.data.to_string_lossy().to_string(),
            ));
        }
        values.push((
            "XDG_DATA_HOME",
            self.integrations.to_string_lossy().to_string(),
        ));
        values.extend(
            self.extra_env
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        );
        if self.umu {
            if self.runner.sandbox {
                let mut command = vec![
                    "flatpak-spawn".into(),
                    "--host".into(),
                    format!(
                        "--directory={}",
                        cwd.unwrap_or(&crate::system::home_dir()).display()
                    ),
                ];
                command.extend(
                    values
                        .iter()
                        .map(|(key, value)| format!("--env={key}={value}")),
                );
                command.push(self.runner.data.join("umu-run").to_string_lossy().into());
                if tool != "wine" {
                    command.push(tool.into());
                }
                command.extend(args.iter().cloned());
                return command;
            }
            let mut command = vec!["env".into()];
            if let Some(cwd) = cwd {
                command.extend(["-C".into(), cwd.to_string_lossy().into()]);
            }
            command.extend(values.iter().map(|(key, value)| format!("{key}={value}")));
            command.push("umu-run".into());
            if tool != "wine" {
                command.push(tool.into());
            }
            command.extend(args.iter().cloned());
            return command;
        }
        if self.flatpak {
            let mut command = vec![
                "flatpak".into(),
                "run".into(),
                format!("--command={tool}"),
                "--filesystem=home".into(),
            ];
            if let Some(cwd) = cwd {
                command.push(format!("--cwd={}", cwd.display()));
            }
            command.extend(
                values
                    .iter()
                    .map(|(key, value)| format!("--env={key}={value}")),
            );
            command.push(self.runtime.clone());
            command.extend(args.iter().cloned());
            return command;
        }
        let mut command = vec!["env".into()];
        if let Some(cwd) = cwd {
            command.extend(["-C".into(), cwd.to_string_lossy().into()]);
        }
        command.extend(values.iter().map(|(key, value)| format!("{key}={value}")));
        command.push(if tool == "wine" {
            self.runtime.clone()
        } else {
            tool.into()
        });
        command.extend(args.iter().cloned());
        command
    }

    fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.prefix)?;
        let runtime = if self.umu { "wineboot.exe" } else { "wineboot" };
        let args = self.command(runtime, &["--init".into()], None);
        self.runner
            .run(&args, self.umu && self.runner.sandbox, true, true)?;
        self.wait()
    }

    fn wait(&self) -> Result<()> {
        // UMU accepts Windows executables, not Wine's Linux-side wineserver
        // utility. umu-run already waits for the Windows process it launched.
        if self.umu {
            return Ok(());
        }
        let args = self.command("wineserver", &["-w".into()], None);
        self.runner
            .run(&args, self.umu && self.runner.sandbox, true, true)?;
        Ok(())
    }
}

fn short_hash(value: &str) -> Result<String> {
    Ok(sha256_bytes(value.as_bytes())?[..20].to_owned())
}

fn parse_windows_environment(input: &str) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(PackliftError::Message(
                "Write each environment variable as NAME=VALUE.".into(),
            ));
        };
        let key = key.trim();
        let value = value.trim();
        let mut chars = key.chars();
        let valid_key = chars
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
        if !valid_key || value.contains(char::is_control) {
            return Err(PackliftError::Message(
                "Use valid environment variable names and single-line values.".into(),
            ));
        }
        if matches!(
            key,
            "WINEPREFIX" | "XDG_DATA_HOME" | "GAMEID" | "PROTONPATH" | "UMU_FOLDERS_PATH"
        ) {
            return Err(PackliftError::Message(format!(
                "Packlift manages {key}; remove it from the custom environment variables."
            )));
        }
        environment.insert(key.to_owned(), value.to_owned());
    }
    Ok(environment)
}

pub fn appimage_offset(path: &Path) -> Result<Option<u64>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 64];
    if file.read_exact(&mut header).is_err()
        || &header[8..11] != b"AI\x02"
        || !matches!(header[4], 1 | 2)
        || !matches!(header[5], 1 | 2)
    {
        return Ok(None);
    }
    let offset = if header[4] == 2 {
        let base = if header[5] == 1 {
            u64::from_le_bytes(header[40..48].try_into().unwrap())
        } else {
            u64::from_be_bytes(header[40..48].try_into().unwrap())
        };
        let entry = if header[5] == 1 {
            u16::from_le_bytes(header[58..60].try_into().unwrap())
        } else {
            u16::from_be_bytes(header[58..60].try_into().unwrap())
        };
        let count = if header[5] == 1 {
            u16::from_le_bytes(header[60..62].try_into().unwrap())
        } else {
            u16::from_be_bytes(header[60..62].try_into().unwrap())
        };
        base.saturating_add(u64::from(entry) * u64::from(count))
    } else {
        let base = if header[5] == 1 {
            u32::from_le_bytes(header[32..36].try_into().unwrap())
        } else {
            u32::from_be_bytes(header[32..36].try_into().unwrap())
        };
        let entry = if header[5] == 1 {
            u16::from_le_bytes(header[46..48].try_into().unwrap())
        } else {
            u16::from_be_bytes(header[46..48].try_into().unwrap())
        };
        let count = if header[5] == 1 {
            u16::from_le_bytes(header[48..50].try_into().unwrap())
        } else {
            u16::from_be_bytes(header[48..50].try_into().unwrap())
        };
        u64::from(base).saturating_add(u64::from(entry) * u64::from(count))
    };
    if offset >= file.metadata()?.len() {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut magic = [0_u8; 4];
    if file.read_exact(&mut magic).is_ok() && &magic == b"hsqs" {
        Ok(Some(offset))
    } else {
        Ok(None)
    }
}

fn pe_icon(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_RESOURCE_BYTES: usize = 32 * 1024 * 1024;
    let mut file = fs::File::open(path)?;
    let mut dos = [0_u8; 64];
    if file.read_exact(&mut dos).is_err() {
        return Ok(None);
    }
    let pe_offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    file.seek(SeekFrom::Start(pe_offset))?;
    let mut coff = [0_u8; 24];
    if file.read_exact(&mut coff).is_err() || &coff[..4] != b"PE\0\0" {
        return Ok(None);
    }
    let section_count = u16::from_le_bytes(coff[6..8].try_into().unwrap()).min(96);
    let optional_size = u16::from_le_bytes(coff[20..22].try_into().unwrap()) as usize;
    if !(120..=1024).contains(&optional_size) {
        return Ok(None);
    }
    let mut optional = vec![0_u8; optional_size];
    file.read_exact(&mut optional)?;
    let magic = u16::from_le_bytes(optional[..2].try_into().unwrap());
    let directory_offset = match magic {
        0x10b => 96,
        0x20b => 112,
        _ => return Ok(None),
    };
    if optional.len() < directory_offset + 24 {
        return Ok(None);
    }
    let resource_rva = u32::from_le_bytes(
        optional[directory_offset + 16..directory_offset + 20]
            .try_into()
            .unwrap(),
    );
    let resource_size = u32::from_le_bytes(
        optional[directory_offset + 20..directory_offset + 24]
            .try_into()
            .unwrap(),
    ) as usize;
    if !(16..=MAX_RESOURCE_BYTES).contains(&resource_size) {
        return Ok(None);
    }
    let mut sections = Vec::new();
    for _ in 0..section_count {
        let mut section = [0_u8; 40];
        if file.read_exact(&mut section).is_err() {
            return Ok(None);
        }
        sections.push((
            u32::from_le_bytes(section[12..16].try_into().unwrap()),
            u32::from_le_bytes(section[8..12].try_into().unwrap())
                .max(u32::from_le_bytes(section[16..20].try_into().unwrap())),
            u32::from_le_bytes(section[16..20].try_into().unwrap()),
            u32::from_le_bytes(section[20..24].try_into().unwrap()),
        ));
    }
    let Some(resource_file_offset) = pe_raw_offset(resource_rva, &sections) else {
        return Ok(None);
    };
    let mut resource = vec![0_u8; resource_size];
    file.seek(SeekFrom::Start(resource_file_offset))?;
    if file.read_exact(&mut resource).is_err() {
        return Ok(None);
    }
    let root = pe_resource_entries(&resource, 0);
    let Some(group_root) = root
        .iter()
        .find(|(id, _)| *id == 14)
        .map(|(_, value)| *value)
    else {
        return Ok(None);
    };
    let Some(icon_root) = root
        .iter()
        .find(|(id, _)| *id == 3)
        .map(|(_, value)| *value)
    else {
        return Ok(None);
    };
    let groups = pe_resource_entries(&resource, group_root & 0x7fff_ffff);
    let icons = pe_resource_entries(&resource, icon_root & 0x7fff_ffff);
    let mut icon_data = std::collections::HashMap::new();
    for (icon_id, target) in icons {
        if let Some(data) = pe_resource_payload(target, &resource, &sections, &mut file)? {
            icon_data.insert(icon_id, data);
        }
    }
    let mut candidates = Vec::new();
    for (_, target) in groups {
        let Some(group) = pe_resource_payload(target, &resource, &sections, &mut file)? else {
            continue;
        };
        if group.len() < 6 {
            continue;
        }
        let reserved = u16::from_le_bytes(group[0..2].try_into().unwrap());
        let kind = u16::from_le_bytes(group[2..4].try_into().unwrap());
        let count = u16::from_le_bytes(group[4..6].try_into().unwrap()) as usize;
        if reserved != 0 || kind != 1 || count == 0 || count > 64 || 6 + count * 14 > group.len() {
            continue;
        }
        for entry in group[6..6 + count * 14].chunks_exact(14) {
            let width = if entry[0] == 0 {
                256
            } else {
                u32::from(entry[0])
            };
            let height = if entry[1] == 0 {
                256
            } else {
                u32::from(entry[1])
            };
            let depth = u16::from_le_bytes(entry[6..8].try_into().unwrap());
            let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let icon_id = u16::from_le_bytes(entry[12..14].try_into().unwrap()) as u32;
            if let Some(data) = icon_data.get(&icon_id) {
                if size > 0 && size <= data.len() && size <= 2 * 1024 * 1024 {
                    candidates.push((width, height, depth, data[..size].to_vec()));
                }
            }
        }
    }
    let Some((width, height, depth, data)) = candidates
        .into_iter()
        .max_by_key(|(width, height, depth, _)| (width * height, *depth))
    else {
        return Ok(None);
    };
    let mut icon = Vec::with_capacity(22 + data.len());
    icon.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    icon.extend_from_slice(&[
        if width == 256 { 0 } else { width as u8 },
        if height == 256 { 0 } else { height as u8 },
        0,
        0,
    ]);
    icon.extend_from_slice(&1_u16.to_le_bytes());
    icon.extend_from_slice(&depth.to_le_bytes());
    icon.extend_from_slice(&(data.len() as u32).to_le_bytes());
    icon.extend_from_slice(&22_u32.to_le_bytes());
    icon.extend_from_slice(&data);
    Ok(Some(icon))
}

fn ico_to_png(icon: &[u8]) -> Option<Vec<u8>> {
    if let Some(png) = png_payload_from_ico(icon) {
        return Some(png.to_vec());
    }
    if icon.len() < 22 || icon[..4] != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes(icon[4..6].try_into().ok()?) as usize;
    let entry = icon.get(6..6 + count.checked_mul(16)?)?;
    let best = entry
        .chunks_exact(16)
        .max_by_key(|entry| u32::from_le_bytes(entry[8..12].try_into().unwrap_or([0; 4])))?;
    let size = u32::from_le_bytes(best[8..12].try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(best[12..16].try_into().ok()?) as usize;
    let dib = icon.get(offset..offset.checked_add(size)?)?;
    if dib.len() < 40 {
        return None;
    }
    let read_i32 = |at| Some(i32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = read_i32(0)?;
    if header < 40 {
        return None;
    }
    let width = read_i32(4)?.unsigned_abs() as usize;
    let height = (read_i32(8)?.unsigned_abs() as usize) / 2;
    let depth = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    if width == 0 || height == 0 || width > 512 || height > 512 || ![24, 32].contains(&depth) {
        return None;
    }
    let bytes_per_pixel = (depth / 8) as usize;
    let xor_stride = (width * bytes_per_pixel + 3) & !3;
    let mask_stride = width.div_ceil(32) * 4;
    let pixels_at = header as usize;
    if pixels_at.checked_add(xor_stride.checked_mul(height)?)? > dib.len() {
        return None;
    }
    let mut rgba = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
    let mut has_alpha = false;
    for y in 0..height {
        let row = pixels_at + (height - 1 - y) * xor_stride;
        for x in 0..width {
            let source = row + x * bytes_per_pixel;
            let target = (y * width + x) * 4;
            rgba[target..target + 3].copy_from_slice(&[
                dib[source + 2],
                dib[source + 1],
                dib[source],
            ]);
            let alpha = if bytes_per_pixel == 4 {
                dib[source + 3]
            } else {
                0
            };
            has_alpha |= alpha != 0;
            rgba[target + 3] = alpha;
        }
    }
    if !has_alpha {
        let mask_at = pixels_at + xor_stride * height;
        if mask_at + mask_stride * height <= dib.len() {
            for y in 0..height {
                for x in 0..width {
                    let source = mask_at + (height - 1 - y) * mask_stride + x / 8;
                    rgba[(y * width + x) * 4 + 3] = if dib[source] & (0x80 >> (x % 8)) == 0 {
                        255
                    } else {
                        0
                    };
                }
            }
        } else {
            for px in rgba.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
    }
    let mut scanlines = Vec::with_capacity((width * 4 + 1) * height);
    for y in 0..height {
        scanlines.push(0);
        for px in rgba[y * width * 4..(y + 1) * width * 4].chunks_exact(4) {
            scanlines.extend_from_slice(px);
        }
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &scanlines).ok()?;
    let compressed = encoder.finish().ok()?;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_png_chunk(&mut png, b"IHDR", &ihdr);
    append_png_chunk(&mut png, b"IDAT", &compressed);
    append_png_chunk(&mut png, b"IEND", &[]);
    Some(png)
}

fn append_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc = !0u32;
    for byte in kind.iter().chain(data) {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb88320
            } else {
                crc >> 1
            };
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

fn png_payload_from_ico(icon: &[u8]) -> Option<&[u8]> {
    if icon.len() < 22 || icon[..4] != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes(icon[4..6].try_into().ok()?) as usize;
    if count == 0 || 6 + count * 16 > icon.len() {
        return None;
    }
    icon[6..6 + count * 16]
        .chunks_exact(16)
        .filter_map(|entry| {
            let size = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            let end = offset.checked_add(size)?;
            let data = icon.get(offset..end)?;
            data.starts_with(b"\x89PNG\r\n\x1a\n").then_some(data)
        })
        .max_by_key(|data| data.len())
}

fn pe_resource_entries(resource: &[u8], offset: u32) -> Vec<(u32, u32)> {
    let offset = offset as usize;
    if offset + 16 > resource.len() {
        return Vec::new();
    }
    let named = u16::from_le_bytes(resource[offset + 12..offset + 14].try_into().unwrap()) as usize;
    let ids = u16::from_le_bytes(resource[offset + 14..offset + 16].try_into().unwrap()) as usize;
    let count = (named + ids).min(4096);
    if offset + 16 + count * 8 > resource.len() {
        return Vec::new();
    }
    resource[offset + 16..offset + 16 + count * 8]
        .chunks_exact(8)
        .filter_map(|entry| {
            let name = u32::from_le_bytes(entry[..4].try_into().unwrap());
            let target = u32::from_le_bytes(entry[4..].try_into().unwrap());
            if name & 0x8000_0000 != 0 {
                None
            } else {
                Some((name & 0x7fff_ffff, target))
            }
        })
        .collect()
}

fn pe_resource_payload(
    target: u32,
    resource: &[u8],
    sections: &[(u32, u32, u32, u32)],
    file: &mut fs::File,
) -> Result<Option<Vec<u8>>> {
    use std::io::{Read, Seek};
    let mut leaves = vec![target];
    for _ in 0..2 {
        let mut next = Vec::new();
        for value in leaves {
            if value & 0x8000_0000 != 0 {
                next.extend(
                    pe_resource_entries(resource, value & 0x7fff_ffff)
                        .into_iter()
                        .map(|(_, child)| child),
                );
            } else {
                next.push(value);
            }
        }
        leaves = next;
    }
    for leaf in leaves {
        if leaf & 0x8000_0000 != 0 {
            continue;
        }
        let offset = leaf as usize;
        if offset + 16 > resource.len() {
            continue;
        }
        let rva = u32::from_le_bytes(resource[offset..offset + 4].try_into().unwrap());
        let size =
            u32::from_le_bytes(resource[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if size == 0 || size > 2 * 1024 * 1024 {
            continue;
        }
        let Some(file_offset) = pe_raw_offset(rva, sections) else {
            continue;
        };
        let mut data = vec![0_u8; size];
        file.seek(std::io::SeekFrom::Start(file_offset))?;
        if file.read_exact(&mut data).is_ok() {
            return Ok(Some(data));
        }
    }
    Ok(None)
}

fn pe_raw_offset(rva: u32, sections: &[(u32, u32, u32, u32)]) -> Option<u64> {
    sections
        .iter()
        .find_map(|(address, span, raw_size, raw_offset)| {
            if *address <= rva && rva < address.saturating_add(*span) && rva - *address < *raw_size
            {
                Some(u64::from(raw_offset.saturating_add(rva - *address)))
            } else {
                None
            }
        })
}

fn valid_package_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && characters.all(|c| c.is_ascii_alphanumeric() || ".+_-".contains(c))
}

fn package_prefix(container: &str) -> Vec<String> {
    if container.is_empty() {
        Vec::new()
    } else {
        vec![
            "toolbox".into(),
            "run".into(),
            "--container".into(),
            container.into(),
        ]
    }
}

fn package_capture(runner: &Runner, container: &str, args: &[String]) -> Result<String> {
    let mut command = package_prefix(container);
    command.extend(args.iter().cloned());
    runner.capture(&command, false)
}

fn package_indexes_are_fresh(runner: &Runner, manager: &str, container: &str) -> Result<bool> {
    let root = match manager {
        "apt" => "/var/lib/apt/lists",
        "dnf" => "/var/cache/dnf",
        "zypper" => "/var/cache/zypp/raw",
        "pacman" => "/var/lib/pacman/sync",
        _ => return Ok(false),
    };
    let script = format!(
        "find {} -type f -printf '%T@\\n' 2>/dev/null | sort -rn | head -n 1",
        crate::system::shell_quote(root)
    );
    let timestamp = package_capture(runner, container, &["sh".into(), "-c".into(), script])?;
    let Ok(timestamp) = timestamp.trim().parse::<f64>() else {
        return Ok(false);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    Ok(now - timestamp < 6.0 * 60.0 * 60.0)
}

fn package_capture_bytes(runner: &Runner, container: &str, args: &[String]) -> Result<Vec<u8>> {
    let mut command = package_prefix(container);
    command.extend(args.iter().cloned());
    runner.capture_bytes(&command, false)
}

fn elevated(runner: &Runner, container: &str, args: &[String]) -> Result<()> {
    let mut command = package_prefix(container);
    if container.is_empty() {
        if !runner.available("pkexec") {
            return Err(PackliftError::Message(
                "Install polkit and start a desktop authentication agent first.".into(),
            ));
        }
        command.push("pkexec".into());
    } else {
        command.push("sudo".into());
    }
    command.extend(args.iter().cloned());
    runner.run(&command, false, true, true)?;
    Ok(())
}

fn package_target(package: &Package, manager: &str) -> String {
    match manager {
        "apt" if package.architecture != "all" => {
            format!("{}:{}", package.identifier, package.architecture)
        }
        "dnf" | "zypper" => format!("{}.{}", package.identifier, package.architecture),
        _ => package.identifier.clone(),
    }
}

fn already_installed(
    runner: &Runner,
    manager: &str,
    target: &str,
    container: &str,
) -> Result<bool> {
    let args: Vec<String> = match manager {
        "apt" => ["dpkg-query", "-W", "-f=${Status}"]
            .into_iter()
            .map(str::to_owned)
            .chain([target.into()])
            .collect(),
        "dnf" | "zypper" => vec!["rpm".into(), "-q".into(), target.into()],
        "pacman" => vec!["pacman".into(), "-Q".into(), target.into()],
        _ => return Ok(false),
    };
    let mut command = package_prefix(container);
    command.extend(args);
    let result = runner.run(&command, false, true, false)?;
    if !result.status.success() {
        return Ok(false);
    }
    if manager == "apt" {
        let status = runner.capture(&command, false)?;
        Ok(status.split_whitespace().last() == Some("installed"))
    } else {
        Ok(true)
    }
}

fn enable_apt_i386(runner: &Runner, container: &str) -> Result<bool> {
    let architecture = package_capture(
        runner,
        container,
        &["dpkg".into(), "--print-architecture".into()],
    )?;
    if architecture.trim() != "amd64" {
        return Ok(false);
    }
    let foreign = package_capture(
        runner,
        container,
        &["dpkg".into(), "--print-foreign-architectures".into()],
    )?;
    if !foreign
        .split_whitespace()
        .any(|architecture| architecture == "i386")
    {
        elevated(
            runner,
            container,
            &["dpkg".into(), "--add-architecture".into(), "i386".into()],
        )?;
        return Ok(true);
    }
    Ok(false)
}

fn check_launcher_libraries(
    runner: &Runner,
    container: &str,
    executable: &str,
    manager: &str,
    repair: bool,
) -> Result<()> {
    // Only invoke the distribution's loader. Scripts and static executables have no loader dependencies.
    let inspect = r#"export LC_ALL=C
exe=$(command -v -- "$1") || exit 1
case "$(od -An -tx1 -N5 -- "$exe" | tr -d ' ')" in
7f454c4602) loader=/lib64/ld-linux-x86-64.so.2; arch=amd64 ;;
7f454c4601) loader=/lib/ld-linux.so.2; arch=i386 ;;
*) exit 0 ;;
esac
if command -v readelf >/dev/null; then
    headers=$(readelf -l -- "$exe") || exit 1
    case "$headers" in
        *"[Requesting program interpreter: $loader]"*) ;;
        *"[Requesting program interpreter:"*) printf '%s\n' "Unsupported program interpreter for $exe" >&2; exit 1 ;;
        *) exit 0 ;;
    esac
fi
echo "$arch"
[ -x "$loader" ] || { echo "LOADER_MISSING"; exit 0; }
"$loader" --verify "$exe" || exit 0
"$loader" --list "$exe" 2>&1 || echo LOADER_FAILED"#;
    let mut attempted = std::collections::HashSet::new();
    let mut file_index_ready = false;
    let pattern = regex::Regex::new(r"(?m)(?:loading shared libraries: |^\s*)([a-zA-Z0-9_+.\-]+\.so(?:\.[a-zA-Z0-9_.\-]+)*)(?:: cannot open shared object file| => not found)").expect("fixed library pattern");
    loop {
        let output = package_capture(
            runner,
            container,
            &[
                "sh".into(),
                "-c".into(),
                inspect.into(),
                "packlift".into(),
                executable.into(),
            ],
        )?;
        let missing_loader = output.lines().any(|line| line == "LOADER_MISSING");
        let missing_library = pattern
            .captures(&output)
            .and_then(|matched| matched.get(1))
            .map(|name| name.as_str());
        if !missing_loader && missing_library.is_none() {
            if output.lines().any(|line| line == "LOADER_FAILED") {
                return Err(PackliftError::Message(format!(
                    "The runtime in {container} cannot load {executable}: {output}"
                )));
            }
            return Ok(());
        }

        let arch = output.lines().next().unwrap_or("");
        let name = missing_library.unwrap_or(if arch == "i386" {
            "ld-linux.so.2"
        } else {
            "ld-linux-x86-64.so.2"
        });
        if !repair || !attempted.insert(name.to_owned()) || attempted.len() > 32 {
            return Err(PackliftError::Message(format!(
                "{container} could not resolve {name} ({arch}) for {executable}. Check the installation log and the container repositories."
            )));
        }
        let (mut command, provider): (Vec<&str>, String) = match manager {
            "apt" => {
                if !missing_loader && !file_index_ready {
                    elevated(
                        runner,
                        container,
                        &[
                            "apt-get".into(),
                            "install".into(),
                            "--no-remove".into(),
                            "-y".into(),
                            "apt-file".into(),
                        ],
                    )?;
                    elevated(
                        runner,
                        container,
                        &["apt-get".into(), "update".into(), "--error-on=any".into()],
                    )?;
                    file_index_ready = true;
                }
                let triplet = if arch == "i386" {
                    "i386-linux-gnu"
                } else {
                    "x86_64-linux-gnu"
                };
                let provider = if missing_loader {
                    "libc6".into()
                } else {
                    let query = format!(r"^(usr/)?lib/{triplet}/{}$", regex::escape(name));
                    unique_library_provider(
                        runner,
                        container,
                        name,
                        &["apt-file", "-a", arch, "-l", "-x", "search", &query],
                    )?
                };
                (
                    vec!["apt-get", "install", "--no-remove", "-y"],
                    format!("{provider}:{arch}"),
                )
            }
            "dnf" | "zypper" => {
                let (command, loader_package) = if manager == "dnf" {
                    (
                        vec!["dnf", "install", "-y"],
                        if arch == "i386" {
                            "glibc.i686"
                        } else {
                            "glibc.x86_64"
                        },
                    )
                } else {
                    (
                        vec!["zypper", "--non-interactive", "install", "--capability"],
                        if arch == "i386" {
                            "glibc-32bit"
                        } else {
                            "glibc"
                        },
                    )
                };
                let provider = if missing_loader {
                    loader_package.into()
                } else if arch == "i386" {
                    name.into()
                } else {
                    format!("{name}()(64bit)")
                };
                (command, provider)
            }
            "pacman" => {
                let provider = if missing_loader {
                    if arch == "i386" {
                        "lib32-glibc"
                    } else {
                        "glibc"
                    }
                    .into()
                } else {
                    if !file_index_ready {
                        elevated(runner, container, &["pacman".into(), "-Fy".into()])?;
                        file_index_ready = true;
                    }
                    let lib_dir = if arch == "i386" {
                        "usr/lib32"
                    } else {
                        "usr/lib"
                    };
                    unique_library_provider(
                        runner,
                        container,
                        name,
                        &["pacman", "-Fq", "--", &format!("{lib_dir}/{name}")],
                    )?
                };
                (
                    vec!["pacman", "-S", "--needed", "--noconfirm", "--"],
                    provider,
                )
            }
            _ => {
                return Err(PackliftError::Message(format!(
                    "Unsupported library resolver for {manager}."
                )))
            }
        };
        runner.write(&format!(
            "Resolving {name} ({arch}) using {provider} in {container}"
        ));
        command.push(&provider);
        elevated(
            runner,
            container,
            &command.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        )
        .map_err(|error| {
            PackliftError::Message(format!(
                "Could not install a provider for {name} ({arch}) in {container}: {error}"
            ))
        })?;
    }
}

fn unique_library_provider(
    runner: &Runner,
    container: &str,
    library: &str,
    query: &[&str],
) -> Result<String> {
    let output = package_capture(
        runner,
        container,
        &query
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| {
        PackliftError::Message(format!(
            "Could not find a provider for {library} in {container}: {error}"
        ))
    })?;
    let packages: std::collections::BTreeSet<_> = output
        .lines()
        .map(str::trim)
        .map(|line| line.rsplit('/').next().unwrap_or(line))
        .filter(|name| valid_package_identifier(name))
        .collect();
    if packages.len() != 1 {
        return Err(PackliftError::Message(format!(
            "Expected one provider for {library} in {container}. Matching packages: {packages:?}"
        )));
    }
    Ok(packages.first().unwrap().to_string())
}

fn system_launchers(
    runner: &Runner,
    package: &Package,
    manager: &str,
    target: &str,
    container: &str,
) -> Result<Vec<SystemLauncher>> {
    let files_command = match manager {
        "apt" => vec!["dpkg-query".into(), "-L".into(), target.into()],
        "dnf" | "zypper" => vec!["rpm".into(), "-ql".into(), target.into()],
        "pacman" => vec!["pacman".into(), "-Qlq".into(), target.into()],
        _ => vec![],
    };
    let files: Vec<String> = package_capture(runner, container, &files_command)?
        .lines()
        .map(str::to_owned)
        .collect();
    let mut launchers = Vec::new();
    for path in files.iter().filter(|path| {
        (path.starts_with("/usr/share/applications/") || path.starts_with("/opt/"))
            && path.ends_with(".desktop")
    }) {
        let content = package_capture(
            runner,
            container,
            &["cat".into(), "--".into(), path.clone()],
        )
        .unwrap_or_default();
        let mut name = String::new();
        let mut icon = String::new();
        let mut exec = String::new();
        let mut terminal = false;
        let mut wm_class = String::new();
        let mut desktop_application = false;
        let mut inside_entry = false;
        for line in content.lines() {
            if line.starts_with('[') {
                inside_entry = line == "[Desktop Entry]";
                continue;
            }
            if !inside_entry {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                match key {
                    "Type" => desktop_application = value == "Application",
                    "Name" => {
                        if name.is_empty() {
                            name = value.to_owned();
                        }
                    }
                    "Exec" => exec = value.to_owned(),
                    "Icon" => icon = value.to_owned(),
                    "StartupWMClass" => wm_class = value.to_string(),
                    "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
                    "NoDisplay" if value.eq_ignore_ascii_case("true") => {
                        desktop_application = false
                    }
                    _ => {}
                }
            }
        }
        if !desktop_application || exec.is_empty() {
            continue;
        }
        let arguments = shlex::split(&exec)
            .unwrap_or_default()
            .into_iter()
            .filter(|arg| !arg.chars().any(|ch| ch == '%'))
            .collect::<Vec<_>>();
        if arguments.is_empty() {
            continue;
        }
        package_capture(runner, container, &["sh".into(),"-c".into(),"command -v -- \"$1\"".into(),"packlift".into(),arguments[0].clone()])
            .map_err(|_| PackliftError::Message(format!("The installed launcher requires {}, which is missing. Reopen the package to repair the installation.",arguments[0])))?;
        if !container.is_empty() {
            check_launcher_libraries(
                runner,
                container,
                &arguments[0],
                manager,
                package.resolve_libraries,
            )?;
        }
        let candidate = files
            .iter()
            .find(|p| {
                Path::new(p)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .is_some_and(|filename| {
                        Path::new(filename)
                            .file_stem()
                            .is_some_and(|stem| stem == std::ffi::OsStr::new(&icon))
                            && matches!(
                                Path::new(filename).extension().and_then(|v| v.to_str()),
                                Some("png" | "svg" | "xpm")
                            )
                            || filename
                                == Path::new(&icon)
                                    .file_name()
                                    .and_then(|v| v.to_str())
                                    .unwrap_or("")
                    })
            })
            .cloned()
            .unwrap_or(icon);
        let mut argv = package_prefix(container);
        argv.extend(arguments);
        launchers.push((
            if name.is_empty() {
                package.name.clone()
            } else {
                name
            },
            argv,
            candidate,
            terminal,
            wm_class,
        ));
    }
    if launchers.is_empty() {
        let executable = files.iter().find(|path| {
            ["/usr/bin", "/usr/local/bin", "/bin"].contains(
                &Path::new(path)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or(""),
            )
        });
        let Some(executable) = executable else {
            return Ok(launchers);
        };
        if !container.is_empty() {
            check_launcher_libraries(
                runner,
                container,
                executable,
                manager,
                package.resolve_libraries,
            )?;
        }
        let mut argv = package_prefix(container);
        argv.push(executable.clone());
        launchers.push((
            package.name.clone(),
            argv,
            String::new(),
            true,
            String::new(),
        ));
    }
    Ok(launchers)
}

fn unpack_archive(path: &Path, destination: &Path) -> Result<Vec<PathBuf>> {
    const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    const MAX_ARCHIVE_ENTRIES: usize = 100_000;

    let mut probe = fs::File::open(path)?;
    let mut magic = [0_u8; 6];
    let count = std::io::Read::read(&mut probe, &mut magic)?;
    drop(probe);
    if magic[..count].starts_with(b"PK\x03\x04") {
        return unpack_zip(path, destination, MAX_UNPACKED_BYTES, MAX_ARCHIVE_ENTRIES);
    }
    let input: Box<dyn std::io::Read> = if magic[..count].starts_with(b"\x1f\x8b") {
        Box::new(flate2::read::GzDecoder::new(fs::File::open(path)?))
    } else if magic[..count].starts_with(b"\xfd7zXZ\0") {
        Box::new(xz2::read::XzDecoder::new(fs::File::open(path)?))
    } else if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".tar.lzma"))
    {
        let stream = xz2::stream::Stream::new_lzma_decoder(256 * 1024 * 1024)
            .map_err(|_| PackliftError::Message("Unsupported LZMA archive.".into()))?;
        Box::new(xz2::read::XzDecoder::new_stream(
            fs::File::open(path)?,
            stream,
        ))
    } else if magic[..count].starts_with(b"\x28\xb5\x2f\xfd") {
        Box::new(zstd::stream::read::Decoder::new(fs::File::open(path)?)?)
    } else if magic[..count].starts_with(b"BZh") {
        Box::new(bzip2::read::BzDecoder::new(fs::File::open(path)?))
    } else {
        Box::new(fs::File::open(path)?)
    };
    let mut archive = tar::Archive::new(input);
    let mut executable_files = Vec::new();
    let mut symlinks = Vec::new();
    let root = destination.canonicalize()?;
    let mut unpacked_bytes = 0_u64;
    let mut entry_count = 0_usize;
    for entry in archive.entries()? {
        if cancellation_requested() {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        let mut entry = entry?;
        entry_count += 1;
        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(PackliftError::Message(
                "The archive contains too many files (limit: 100,000).".into(),
            ));
        }
        let relative = entry.path()?.into_owned();
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            return Err(PackliftError::Message(
                "The archive contains an invalid file name.".into(),
            ));
        }
        let target = destination.join(&relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if kind.is_symlink() {
            let Some(link) = entry.link_name()? else {
                return Err(PackliftError::Message(
                    "The archive contains an invalid symbolic link.".into(),
                ));
            };
            resolve_archive_link(&relative, &link).ok_or_else(|| {
                PackliftError::Message(
                    "The archive contains a symbolic link that points outside its folder.".into(),
                )
            })?;
            symlinks.push((target, link.into_owned()));
            continue;
        }
        if !kind.is_file() {
            return Err(PackliftError::Message(
                "The archive contains a link or special file. Use an AppImage or a system package."
                    .into(),
            ));
        }
        let size = entry.header().size()?;
        unpacked_bytes = unpacked_bytes.checked_add(size).ok_or_else(|| {
            PackliftError::Message("The archive expands beyond Packlift's 4 GiB limit.".into())
        })?;
        if unpacked_bytes > MAX_UNPACKED_BYTES {
            return Err(PackliftError::Message(
                "The archive expands beyond Packlift's 4 GiB limit.".into(),
            ));
        }
        let parent = target.parent().ok_or_else(|| {
            PackliftError::Message("The archive contains an invalid file name.".into())
        })?;
        fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&root) {
            return Err(PackliftError::Message(
                "The archive contains a path that escapes its destination.".into(),
            ));
        }
        let mode = entry.header().mode().unwrap_or(0);
        entry.unpack(&target)?;
        if mode & 0o111 != 0 {
            let mut header = [0_u8; 20];
            let count = fs::File::open(&target)
                .and_then(|mut file| std::io::Read::read(&mut file, &mut header))
                .unwrap_or(0);
            if header[..count].starts_with(b"#!")
                || !crate::core::elf_architecture(&header[..count]).is_empty()
            {
                executable_files.push(target);
            }
        }
        if executable_files.len() > 1024 {
            return Err(PackliftError::Message(
                "The archive contains too many executable files.".into(),
            ));
        }
    }
    for (path, link) in symlinks {
        let parent = path.parent().ok_or_else(|| {
            PackliftError::Message("The archive contains an invalid symbolic link.".into())
        })?;
        fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&root)
            || fs::symlink_metadata(&path).is_ok()
            || executable_files.iter().any(|file| file.starts_with(&path))
        {
            return Err(PackliftError::Message(
                "The archive contains conflicting symbolic links.".into(),
            ));
        }
        std::os::unix::fs::symlink(link, path)?;
    }
    Ok(executable_files)
}

fn unpack_zip(
    path: &Path,
    destination: &Path,
    max_unpacked_bytes: u64,
    max_entries: usize,
) -> Result<Vec<PathBuf>> {
    let root = destination.canonicalize()?;
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)
        .map_err(|error| PackliftError::Message(format!("Could not read ZIP archive: {error}")))?;
    if archive.len() > max_entries {
        return Err(PackliftError::Message(
            "The archive contains too many files (limit: 100,000).".into(),
        ));
    }
    let mut total = 0_u64;
    let mut executables = Vec::new();
    let mut symlinks = Vec::<(PathBuf, PathBuf)>::new();
    for index in 0..archive.len() {
        if cancellation_requested() {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        let entry = archive.by_index(index).map_err(|error| {
            PackliftError::Message(format!("Could not read ZIP entry: {error}"))
        })?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| {
                PackliftError::Message("The archive contains an invalid file name.".into())
            })?
            .to_owned();
        if relative.as_os_str().is_empty() {
            return Err(PackliftError::Message(
                "The archive contains an invalid file name.".into(),
            ));
        }
        let target = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        let declared_size = entry.size();
        let total_with_declared = total.checked_add(declared_size).ok_or_else(|| {
            PackliftError::Message("The archive expands beyond Packlift's 4 GiB limit.".into())
        })?;
        if total_with_declared > max_unpacked_bytes {
            return Err(PackliftError::Message(
                "The archive expands beyond Packlift's 4 GiB limit.".into(),
            ));
        }
        let mode = entry.unix_mode().unwrap_or(0);
        let file_type = mode & 0o170000;
        if file_type == 0o120000 {
            const MAX_SYMLINK_TARGET_BYTES: u64 = 4096;
            if declared_size > MAX_SYMLINK_TARGET_BYTES {
                return Err(PackliftError::Message(
                    "The archive contains an oversized symbolic link.".into(),
                ));
            }
            let mut target_bytes = Vec::new();
            entry
                .take(MAX_SYMLINK_TARGET_BYTES + 1)
                .read_to_end(&mut target_bytes)?;
            if target_bytes.len() as u64 > MAX_SYMLINK_TARGET_BYTES {
                return Err(PackliftError::Message(
                    "The archive contains an oversized symbolic link.".into(),
                ));
            }
            let target_text = String::from_utf8(target_bytes).map_err(|_| {
                PackliftError::Message("The archive contains an invalid symbolic link.".into())
            })?;
            let link = PathBuf::from(target_text);
            resolve_archive_link(&relative, &link).ok_or_else(|| {
                PackliftError::Message(
                    "The archive contains a symbolic link that points outside its folder.".into(),
                )
            })?;
            symlinks.push((target, link));
            total = total_with_declared;
            continue;
        }
        if file_type != 0 && file_type != 0o100000 {
            return Err(PackliftError::Message(
                "The archive contains a special file.".into(),
            ));
        }
        let parent = target.parent().ok_or_else(|| {
            PackliftError::Message("The archive contains an invalid file name.".into())
        })?;
        fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&root) {
            return Err(PackliftError::Message(
                "The archive contains a path that escapes its destination.".into(),
            ));
        }
        let mut output = fs::File::create(&target)?;
        let copied = std::io::copy(
            &mut entry.take(max_unpacked_bytes.saturating_sub(total).saturating_add(1)),
            &mut output,
        )?;
        if copied > max_unpacked_bytes.saturating_sub(total) || copied != declared_size {
            return Err(PackliftError::Message(
                "The archive expands beyond Packlift's 4 GiB limit.".into(),
            ));
        }
        total = total.saturating_add(copied);
        if mode & 0o111 != 0 {
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777))?;
            let mut header = [0_u8; 20];
            let count = fs::File::open(&target)?.read(&mut header)?;
            if header[..count].starts_with(b"#!")
                || !crate::core::elf_architecture(&header[..count]).is_empty()
            {
                executables.push(target);
            }
        }
        if executables.len() > 1024 {
            return Err(PackliftError::Message(
                "The archive contains too many executable files.".into(),
            ));
        }
    }
    for (path, link) in symlinks {
        let parent = path.parent().ok_or_else(|| {
            PackliftError::Message("The archive contains an invalid symbolic link.".into())
        })?;
        fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&root) || fs::symlink_metadata(&path).is_ok() {
            return Err(PackliftError::Message(
                "The archive contains conflicting symbolic links.".into(),
            ));
        }
        std::os::unix::fs::symlink(link, path)?;
    }
    Ok(executables)
}

fn resolve_archive_link(link_path: &Path, link: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if link.is_absolute() {
        return None;
    }
    let mut components = Vec::new();
    for component in link_path.parent()?.components() {
        match component {
            Component::Normal(value) => components.push(value.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    for component in link.components() {
        match component {
            Component::Normal(value) => components.push(value.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop()?;
            }
            _ => return None,
        }
    }
    Some(components.into_iter().collect())
}

type ExecutableSnapshot = BTreeMap<PathBuf, (u64, i64, i64)>;
type WindowsRegistrations = BTreeMap<(PathBuf, String), WindowsRegistration>;

#[derive(Default, PartialEq)]
struct WindowsRegistration {
    name: String,
    icon: String,
    location: String,
}

fn windows_search_roots(prefix: &Path, home: &Path, installer_dir: &Path) -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    for drive in [prefix.join("drive_c"), prefix.join("pfx/drive_c")] {
        for directory in ["Program Files", "Program Files (x86)", "Games"] {
            roots.insert(drive.join(directory));
        }
    }
    if home != Path::new("/") {
        roots.extend(["Games", "games", "Jogos"].map(|directory| home.join(directory)));
    }
    if let (Ok(home), Ok(directory)) = (home.canonicalize(), installer_dir.canonicalize()) {
        if !home.starts_with(&directory) {
            roots.insert(directory);
        }
    }
    roots
}

fn windows_executables(roots: &BTreeSet<PathBuf>) -> Result<ExecutableSnapshot> {
    let mut snapshot = ExecutableSnapshot::new();
    let mut remaining = 60_000;
    for root in roots {
        collect_exes(root, &mut snapshot, &mut remaining, 0)?;
    }
    Ok(snapshot)
}

fn windows_registrations(prefix: &Path) -> WindowsRegistrations {
    let mut registrations = WindowsRegistrations::new();
    for directory in [prefix.to_path_buf(), prefix.join("pfx")] {
        for name in ["system.reg", "user.reg"] {
            let file = directory.join(name);
            let Ok(contents) = fs::read_to_string(&file) else {
                continue;
            };
            let mut registration = None;
            for line in contents.lines().map(str::trim) {
                if let Some((key, _)) = line.strip_prefix('[').and_then(|s| s.split_once(']')) {
                    registration = key
                        .to_ascii_lowercase()
                        .contains(r"\\uninstall\\")
                        .then(|| {
                            registrations
                                .entry((file.clone(), key.to_owned()))
                                .or_default()
                        });
                    continue;
                }
                let (Some(entry), Some((property, value))) =
                    (registration.as_deref_mut(), line.split_once('='))
                else {
                    continue;
                };
                let field = match property {
                    r#""DisplayName""# => &mut entry.name,
                    r#""DisplayIcon""# => &mut entry.icon,
                    r#""InstallLocation""# => &mut entry.location,
                    _ => continue,
                };
                if let Some(value) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    *field = value.replace(r#"\""#, "\"").replace(r"\\", "\\");
                }
            }
        }
    }
    registrations
}

fn windows_registry_path(prefix: &Path, value: &str) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"');
    let (drive, path) = value.split_once(':')?;
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    if !path.starts_with(['\\', '/']) {
        return None;
    }
    let drive = drive.to_ascii_lowercase();
    let mut root = prefix.join("dosdevices").join(format!("{drive}:"));
    if !root.is_dir() && drive == "c" {
        root = prefix.join("drive_c");
    }
    let mut path = path.split(['\\', '/']).filter(|part| !part.is_empty());
    root.push(path.next().filter(|part| !matches!(*part, "." | ".."))?);
    for part in path {
        if matches!(part, "." | "..") {
            return None;
        }
        root.push(part);
    }
    root.canonicalize().ok()
}

fn find_installed_executable(
    package: &Package,
    prefix: &Path,
    registrations: &WindowsRegistrations,
    roots: &BTreeSet<PathBuf>,
    before: Option<ExecutableSnapshot>,
) -> Option<PathBuf> {
    let setup = package.path.canonicalize().ok();
    let viable = |path: &Path| {
        Some(path) != setup.as_deref()
            && is_auto_executable(path)
            && matches!(detect(path), Ok((kind, _)) if kind == "exe")
    };
    let mut registered_targets = BTreeSet::new();
    let mut remaining = 60_000;
    for (key, entry) in windows_registrations(prefix) {
        if registrations.get(&key) == Some(&entry) {
            continue;
        }
        let directory = key.0.parent()?;
        let icon = entry
            .icon
            .rsplit_once(',')
            .filter(|(_, index)| index.trim().parse::<i32>().is_ok())
            .map_or(entry.icon.as_str(), |(path, _)| path);
        if let Some(target) = windows_registry_path(directory, icon).filter(|path| viable(path)) {
            registered_targets.insert(target);
            continue;
        }
        let Some(location) = windows_registry_path(directory, &entry.location) else {
            continue;
        };
        let mut candidates = ExecutableSnapshot::new();
        if collect_exes(&location, &mut candidates, &mut remaining, 0).is_ok() {
            if let Some(target) = select_executable(
                candidates.into_keys().filter(|path| viable(path)),
                &entry.name,
            ) {
                registered_targets.insert(target);
            }
        }
    }
    if !registered_targets.is_empty() {
        return select_executable(registered_targets.into_iter(), &package.name);
    }
    let before = before?;
    let after = windows_executables(roots).ok()?;
    select_executable(
        after.into_iter().filter_map(|(path, metadata)| {
            (before.get(&path) != Some(&metadata) && viable(&path)).then_some(path)
        }),
        &package.name,
    )
}

fn select_executable(candidates: impl Iterator<Item = PathBuf>, name: &str) -> Option<PathBuf> {
    fn normalized(value: &str) -> String {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    let name = normalized(name);
    let mut best = None;
    let mut best_score = None;
    for path in candidates {
        let stem = normalized(&path.file_stem().unwrap_or_default().to_string_lossy());
        let score = if name.chars().count() >= 4 && stem == name {
            2
        } else if stem.chars().count() >= 4
            && path
                .ancestors()
                .skip(1)
                .take(4)
                .filter_map(Path::file_name)
                .any(|directory| normalized(&directory.to_string_lossy()) == stem)
        {
            1
        } else {
            0
        };
        if best_score.is_none_or(|previous| score > previous) {
            best_score = Some(score);
            best = Some(path);
        } else if best_score == Some(score) {
            best = None;
        }
    }
    best
}

fn is_program_executable(path: &Path) -> bool {
    let Some(extension) = path.extension() else {
        return false;
    };
    let Some(stem) = path.file_stem() else {
        return false;
    };
    let name = stem.to_string_lossy().to_ascii_lowercase();
    extension.eq_ignore_ascii_case("exe")
        && !matches!(
            name.as_str(),
            "setup"
                | "install"
                | "installer"
                | "uninstall"
                | "uninstaller"
                | "updater"
                | "crashreporter"
                | "crashreportclient"
                | "crashpad_handler"
        )
        && !name.starts_with("unins00")
}

fn is_auto_executable(path: &Path) -> bool {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let stem = stem.to_ascii_lowercase();
    is_program_executable(path)
        && !matches!(
            stem.as_str(),
            "dxsetup" | "unitycrashhandler64" | "unitycrashhandler32" | "ue4prereqsetup_x64"
        )
        && !stem.starts_with("vc_redist")
        && !stem.starts_with("vcredist")
}

fn collect_exes(
    directory: &Path,
    output: &mut ExecutableSnapshot,
    remaining: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > 10 || !directory.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        *remaining = remaining.checked_sub(1).ok_or_else(|| {
            PackliftError::Message("There are too many files to inspect for executables.".into())
        })?;
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            collect_exes(&path, output, remaining, depth + 1)?;
        } else if metadata.is_file() && is_auto_executable(&path) {
            let metadata = entry.metadata()?;
            output.insert(
                path.canonicalize()?,
                (metadata.len(), metadata.ctime(), metadata.ctime_nsec()),
            );
        }
    }
    Ok(())
}

fn copy_binary_payload(
    source: &Path,
    destination: &Path,
    include_neighbors: bool,
) -> Result<PathBuf> {
    const MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
    const MAX_PAYLOAD_FILES: usize = 256;

    let parent = source
        .parent()
        .ok_or_else(|| PackliftError::Message("The executable has no parent folder.".into()))?
        .canonicalize()?;
    let source_real = source.canonicalize()?;
    if !source_real.starts_with(&parent) || !source_real.is_file() {
        return Err(PackliftError::Message(
            "The selected executable must point to a file inside its folder.".into(),
        ));
    }
    let name = source.file_name().ok_or_else(|| {
        PackliftError::Message("The selected executable has no file name.".into())
    })?;
    let target = destination.join(name);
    if cancellation_requested() {
        return Err(PackliftError::Message("Operation canceled.".into()));
    }
    let source_size = fs::metadata(&source_real)?.len();
    if source_size > MAX_PAYLOAD_BYTES {
        return Err(PackliftError::Message(
            "This executable is larger than Packlift's 512 MiB import limit.".into(),
        ));
    }
    fs::copy(&source_real, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;

    if !include_neighbors {
        return Ok(target);
    }

    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut total_size = source_size;
    for entry in fs::read_dir(&parent)? {
        if cancellation_requested() {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        let entry = entry?;
        if entry.file_name() == name {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_file() {
            let size = entry.metadata()?.len();
            total_size = total_size.checked_add(size).ok_or_else(|| {
                PackliftError::Message("The selected folder is too large to import.".into())
            })?;
            files.push((path, entry.file_name(), size));
        } else if kind.is_symlink() {
            let Ok(link) = fs::read_link(&path) else {
                continue;
            };
            if link.is_absolute()
                || link
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                continue;
            }
            if fs::canonicalize(&path)
                .is_ok_and(|resolved| resolved.starts_with(&parent) && resolved.is_file())
            {
                links.push((entry.file_name(), link));
            }
        }
        if files.len() + links.len() > MAX_PAYLOAD_FILES {
            return Err(PackliftError::Message(
                "The selected folder contains too many files to import (limit: 256).".into(),
            ));
        }
        if total_size > MAX_PAYLOAD_BYTES {
            return Err(PackliftError::Message(
                "Nearby files exceed Packlift's 512 MiB import limit.".into(),
            ));
        }
    }
    for (path, name, _) in files {
        if cancellation_requested() {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        let target = destination.join(name);
        fs::copy(&path, &target)?;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
    }
    for (name, link) in links {
        let target = destination.join(name);
        if !target.exists() {
            std::os::unix::fs::symlink(link, target)?;
        }
    }
    Ok(target)
}

fn find_dir_binary(dir: &Path) -> Option<PathBuf> {
    let dir_name = dir.file_name()?.to_str()?;
    let entries = fs::read_dir(dir).ok()?;
    let mut fallback = None;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() {
            if let Ok(mut f) = fs::File::open(&p) {
                let mut h = [0u8; 4];
                if let Ok(4) = f.read(&mut h) {
                    if &h == b"\x7fELF" {
                        let fname = p.file_name()?.to_str()?;
                        if fname.eq_ignore_ascii_case(dir_name)
                            || dir_name
                                .to_ascii_lowercase()
                                .starts_with(&fname.to_ascii_lowercase())
                        {
                            return Some(p);
                        }
                        if fallback.is_none() {
                            fallback = Some(p);
                        }
                    }
                }
            }
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::{desktop_exec, ico_to_png, windows_desktop_command};

    #[test]
    fn window_shortcuts_keep_the_menu_icon_and_follow_wine_and_appimage_identities() {
        use super::{fs, json, remove_exports, Installer};
        use std::sync::Arc;

        let root = std::env::temp_dir().join(format!("packlift-window-id-{}", std::process::id()));
        let applications = root.join("applications");
        let installer = Installer::new(
            Arc::new(|_| {}),
            Some(root.join("data")),
            Some(applications.clone()),
        )
        .unwrap();
        for (method, window_id, class) in [
            ("wine", "packliftdemogame.exe", "packliftdemogame.exe"),
            ("local", "org.example.PackliftAppImage", "PackliftDemoApp"),
        ] {
            let desktop = applications.join(format!("{method}.desktop"));
            let alias = applications.join(format!("{window_id}.desktop"));
            let mut record = json!({"id":method,"method":method,"exports":[{
                "desktop":desktop, "name":"Demo", "icon":"demo-icon", "argv":["/usr/bin/true"],
                "window_id":window_id, "startup_wm_class":class, "wine_args":["PackliftDemoGame.exe"],
            }]});
            installer.sync_app_menu_entry(&mut record).unwrap();
            assert!(fs::read_to_string(&desktop)
                .unwrap()
                .contains(&format!("StartupWMClass={class}\n")));
            let shortcut = fs::read_to_string(&alias).unwrap();
            assert!(shortcut.contains("NoDisplay=true\n"));
            assert!(shortcut.contains(if method == "wine" {
                "Icon=application-x-executable\n"
            } else {
                "Icon=demo-icon\n"
            }));
            assert_eq!(
                record["exports"][0]["window_desktop"].as_str(),
                alias.to_str()
            );
            if method == "wine" {
                remove_exports(&record, &applications, &installer.data);
                assert!(!alias.exists());
            } else {
                fs::write(&alias, "[Desktop Entry]\nName=Another installed app\n").unwrap();
                installer.sync_app_menu_entry(&mut record).unwrap();
                remove_exports(&record, &applications, &installer.data);
                assert!(fs::read_to_string(&alias)
                    .unwrap()
                    .contains("Name=Another installed app"));
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flatpak_run_passes_architecture_and_branch_as_option_values() {
        let argv =
            super::flatpak_launch_command("--user", "org.mozilla.firefox", "x86_64", "stable");
        assert_eq!(
            argv,
            [
                "flatpak",
                "run",
                "--user",
                "--arch=x86_64",
                "--branch=stable",
                "--",
                "org.mozilla.firefox"
            ]
        );
    }

    #[test]
    fn flatpak_list_preserves_the_user_app_ref_and_commit() {
        use super::flatpak_apps_from_list;

        let apps = flatpak_apps_from_list(concat!(
            "Installation\tRef\tActive commit\tName\tVersion\n",
            "user\torg.example.Demo/x86_64/stable\tcommit1\tDemo App\t1.0\n",
            "system (games)\torg.example.Demo/x86_64/beta\tcommit2\t\t\n",
        ));
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].2.name, "Demo App");
        assert_eq!(apps[0].2.version, "1.0");
        assert_eq!(apps[1].0, "system (games)");
        assert_eq!(apps[1].2.commit, "commit2");
    }

    #[test]
    fn prefix_folder_uses_the_drive_for_the_selected_runtime() {
        use super::{fs, windows_prefix_drive};

        let data =
            std::env::temp_dir().join(format!("packlift-prefix-test-{}", std::process::id()));
        let wine_drive = data.join("wine/prefix/drive_c");
        let proton_drive = data.join("wine/prefix/pfx/drive_c");
        fs::create_dir_all(&wine_drive).unwrap();
        assert_eq!(windows_prefix_drive(&data, "proton"), wine_drive);
        fs::create_dir_all(&proton_drive).unwrap();
        assert_eq!(windows_prefix_drive(&data, "proton"), proton_drive);
        assert_eq!(windows_prefix_drive(&data, "wine"), wine_drive);
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn windows_setup_finds_a_game_on_a_proton_mapped_drive() {
        use super::{
            find_installed_executable, fs, windows_executables, windows_registrations,
            windows_search_roots, Package,
        };
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("packlift-setup-reg-{}", std::process::id()));
        let prefix = root.join("prefix");
        let proton = prefix.join("pfx");
        let home = root.join("home");
        let drive = root.join("external");
        let game = drive.join("Library/My Game");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(proton.join("dosdevices")).unwrap();
        fs::create_dir_all(&game).unwrap();
        symlink(&drive, proton.join("dosdevices/d:")).unwrap();
        let setup = home.join("Setup.exe");
        write_windows_executable(&setup);
        let package = Package {
            name: "Setup".into(),
            path: setup,
            ..Package::default()
        };
        let roots = windows_search_roots(&prefix, &home, &home);
        let registrations = windows_registrations(&prefix);
        let before = windows_executables(&roots).ok();
        let target = game.join("My Game.exe");
        write_windows_executable(&target);
        write_windows_executable(&game.join("Benchmark.exe"));
        let registry = proton.join("system.reg");
        let section = r"[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Game] 123";
        for hint in [
            r#""DisplayIcon"="\"D:\\Library\\My Game\\My Game.exe\",0""#,
            r#""InstallLocation"="D:\\Library\\My Game""#,
        ] {
            fs::write(
                &registry,
                format!(
                    "WINE REGISTRY Version 2\n{section}\n\"DisplayName\"=\"My Game\"\n{hint}\n"
                ),
            )
            .unwrap();
            assert_eq!(
                find_installed_executable(
                    &package,
                    &prefix,
                    &registrations,
                    &roots,
                    before.clone()
                ),
                Some(target.canonicalize().unwrap())
            );
        }
        let registrations = windows_registrations(&prefix);
        assert_eq!(
            find_installed_executable(&package, &prefix, &registrations, &roots, before),
            None,
            "ignore an unchanged uninstall entry"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn windows_setup_compares_game_files_and_only_breaks_a_tie_with_a_name_match() {
        use super::{
            find_installed_executable, fs, windows_executables, windows_search_roots, Package,
        };

        let root = std::env::temp_dir().join(format!("packlift-setup-scan-{}", std::process::id()));
        let prefix = root.join("prefix");
        let home = root.join("home");
        let games = home.join("Jogos");
        let bin = games.join("My Game/bin/x64");
        fs::create_dir_all(&bin).unwrap();
        write_windows_executable(&games.join("Old Game.exe"));
        let setup = home.join("Setup.exe");
        write_windows_executable(&setup);
        let package = Package {
            name: "Setup".into(),
            path: setup,
            ..Package::default()
        };
        let roots = windows_search_roots(&prefix, &home, &home);
        let before = windows_executables(&roots).unwrap();
        let target = bin.join("MyGame.exe");
        write_windows_executable(&target);
        write_windows_executable(&bin.join("vc_redist.x64.exe"));
        let find = || {
            find_installed_executable(
                &package,
                &prefix,
                &Default::default(),
                &roots,
                Some(before.clone()),
            )
        };
        assert_eq!(find(), Some(target.canonicalize().unwrap()));
        write_windows_executable(&bin.join("Editor.exe"));
        assert_eq!(find(), Some(target.canonicalize().unwrap()));
        write_windows_executable(&games.join("My Game/My Game.exe"));
        assert_eq!(find(), None);
        fs::remove_dir_all(root).unwrap();
    }

    fn write_windows_executable(path: &super::Path) {
        let mut header = [0_u8; 88];
        header[..2].copy_from_slice(b"MZ");
        header[60] = 64;
        header[64..70].copy_from_slice(&[b'P', b'E', 0, 0, 0x64, 0x86]);
        super::fs::write(path, header).unwrap();
    }

    #[test]
    fn windows_setup_registers_the_selected_executable_and_its_own_identity() {
        use super::{fs, Installer, Package, Wine};
        use std::sync::Arc;

        let temp =
            std::env::temp_dir().join(format!("packlift-setup-target-test-{}", std::process::id()));
        let games = temp.join("Games");
        fs::create_dir_all(&games).unwrap();
        let installer = Installer::new(
            Arc::new(|_| {}),
            Some(temp.join("data")),
            Some(temp.join("applications")),
        )
        .unwrap();
        let source = temp.join("Setup.setup");
        fs::write(&source, b"installer").unwrap();
        let package = Package {
            name: "Setup".into(),
            path: source,
            kind: "setup".into(),
            icon: Some(temp.join("setup-icon.png")),
            ..Package::default()
        };
        let target = games.join("My Game.exe");
        write_windows_executable(&target);
        let wine = Wine::new(
            installer.runner.clone(),
            "wine".into(),
            &installer.data,
            &installer.applications,
            "x86_64",
        );
        let record = installer
            .register_windows_target(&package, &wine, "sample", &target)
            .unwrap();
        assert_eq!(record["name"], "My Game");
        assert_eq!(record["exports"][0]["icon"], "application-x-executable");
        assert_eq!(
            record["exports"][0]["wine_args"][0].as_str(),
            target.canonicalize().unwrap().to_str()
        );
        assert!(
            record["windows_directory"].is_null(),
            "removing a shortcut must not delete external game files"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn selected_proton_version_reaches_the_host_shortcut() {
        use super::{Path, PathBuf, Runner, Wine};
        use std::sync::Arc;

        let runner = Runner::new(
            Some(std::env::temp_dir().join("packlift-runtime-command-test")),
            Arc::new(|_| {}),
        )
        .unwrap();
        let mut simulated_sandbox = runner.clone();
        simulated_sandbox.sandbox = true;
        let mut wine = Wine::new(
            simulated_sandbox,
            "proton".into(),
            &runner.data,
            &runner.data,
            "x86_64",
        );
        wine.proton_path = Some(PathBuf::from("/home/user/Proton Versions/GE-Proton"));
        let argv = wine.command(
            "wine",
            &["/home/user/Games/My Game.exe".into()],
            Some(Path::new("/home/user/Games")),
        );
        assert!(argv
            .iter()
            .any(|arg| arg == "--env=PROTONPATH=/home/user/Proton Versions/GE-Proton"));
        let shortcut = windows_desktop_command(&argv);
        assert!(shortcut
            .iter()
            .any(|arg| arg == "PROTONPATH=/home/user/Proton Versions/GE-Proton"));
        assert!(desktop_exec(&shortcut)
            .contains(r#""PROTONPATH=/home/user/Proton Versions/GE-Proton""#));
    }

    #[test]
    fn sandbox_archive_registers_launches_and_removes_without_a_desktop_file() {
        use super::{fs, Installer, Package};
        use std::sync::Arc;

        let root = std::env::temp_dir().join(format!(
            "packlift-sandbox-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("sample.tar");
        let mut archive = tar::Builder::new(fs::File::create(&source).unwrap());
        archive
            .append_path_with_name("/usr/bin/true", "sample")
            .unwrap();
        archive.finish().unwrap();

        let applications = root.join("applications");
        let installer = Installer::new(
            Arc::new(|_| {}),
            Some(root.join("data")),
            Some(applications.clone()),
        )
        .unwrap();
        let directory = installer.data.join("installed/sample");
        let package = Package {
            path: source,
            kind: "tar".into(),
            name: "Sample".into(),
            ..Package::default()
        };
        let record = installer
            .install_local_payload(&package, std::env::consts::ARCH, "sample", &directory, true)
            .unwrap();

        assert!(record["exports"][0]["desktop"].is_null());
        assert!(!applications.exists());
        assert_eq!(installer.registry.apps().unwrap().len(), 1);
        installer.launch(&record).unwrap();
        installer.uninstall(&record).unwrap();
        assert!(!directory.exists());
        assert!(installer.registry.apps().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_archives_extract_executables_and_reject_parent_traversal() {
        use super::{fs, unpack_archive};
        use std::io::Write;

        let root = std::env::temp_dir().join(format!(
            "packlift-zip-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("portable.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "Game/Game",
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        archive.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        archive.finish().unwrap();

        assert_eq!(
            crate::core::detect(&archive_path).unwrap().0,
            "tar",
            "portable ZIPs use the same local archive installer path"
        );
        let destination = root.join("payload");
        fs::create_dir(&destination).unwrap();
        let executable = unpack_archive(&archive_path, &destination).unwrap();
        assert_eq!(executable, vec![destination.join("Game/Game")]);

        let traversal_path = root.join("unsafe.zip");
        let mut archive = zip::ZipWriter::new(fs::File::create(&traversal_path).unwrap());
        archive
            .start_file("../outside", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"unsafe").unwrap();
        archive.finish().unwrap();
        assert!(unpack_archive(&traversal_path, &destination).is_err());
        assert!(!root.join("outside").exists());

        let link_path = root.join("unsafe-link.zip");
        let mut archive = zip::ZipWriter::new(fs::File::create(&link_path).unwrap());
        archive
            .start_file(
                "Game/link",
                zip::write::SimpleFileOptions::default().external_attributes(0o120777 << 16),
            )
            .unwrap();
        archive.write_all(b"../../outside").unwrap();
        archive.finish().unwrap();
        assert!(unpack_archive(&link_path, &destination).is_err());
        assert!(!root.join("outside").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lzma_tar_archives_are_detected_and_extracted_within_the_memory_limit() {
        use super::{fs, unpack_archive};

        let root = std::env::temp_dir().join(format!(
            "packlift-lzma-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&root).unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        let contents = b"#!/bin/sh\nexit 0\n";
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "Game/Game", &contents[..])
            .unwrap();
        let tar_bytes = archive.into_inner().unwrap();
        let options = xz2::stream::LzmaOptions::new_preset(1).unwrap();
        let stream = xz2::stream::Stream::new_lzma_encoder(&options).unwrap();
        let mut encoder = xz2::write::XzEncoder::new_stream(Vec::new(), stream);
        use std::io::Write;
        encoder.write_all(&tar_bytes).unwrap();
        let archive_path = root.join("game.tar.lzma");
        fs::write(&archive_path, encoder.finish().unwrap()).unwrap();

        assert_eq!(crate::core::detect(&archive_path).unwrap().0, "tar");
        let destination = root.join("payload");
        fs::create_dir(&destination).unwrap();
        let executables = unpack_archive(&archive_path, &destination).unwrap();
        assert_eq!(executables, vec![destination.join("Game/Game")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn proton_menu_shortcut_uses_a_host_command_and_preserves_paths() {
        let argv = [
            "flatpak-spawn",
            "--host",
            "--directory=/home/user/Game Files",
            "--env=WINEPREFIX=/home/user/Proton Prefix",
            "/home/user/.local/share/flatapk/umu-run",
            "/home/user/Game Files/Game.exe",
        ]
        .map(str::to_owned);

        assert_eq!(
            desktop_exec(&windows_desktop_command(&argv)),
            r#"/usr/bin/env -C "/home/user/Game Files" "WINEPREFIX=/home/user/Proton Prefix" /home/user/.local/share/flatapk/umu-run "/home/user/Game Files/Game.exe""#
        );
    }

    #[test]
    fn converts_legacy_bmp_ico_to_png_for_linux_menus() {
        let mut icon = vec![0, 0, 1, 0, 1, 0];
        icon.extend_from_slice(&[1, 1, 0, 0, 1, 0, 32, 0]);
        icon.extend_from_slice(&48u32.to_le_bytes());
        icon.extend_from_slice(&22u32.to_le_bytes());
        icon.extend_from_slice(&40u32.to_le_bytes());
        icon.extend_from_slice(&1i32.to_le_bytes());
        icon.extend_from_slice(&2i32.to_le_bytes());
        icon.extend_from_slice(&1u16.to_le_bytes());
        icon.extend_from_slice(&32u16.to_le_bytes());
        icon.extend_from_slice(&0u32.to_le_bytes());
        icon.extend_from_slice(&4u32.to_le_bytes());
        icon.extend_from_slice(&[0; 16]);
        icon.extend_from_slice(&[20, 40, 200, 255]);
        icon.extend_from_slice(&[0; 4]);

        let png = ico_to_png(&icon).expect("supported BMP icon should convert");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn local_binary_payload_registers_and_creates_desktop_shortcut() {
        use super::{fs, Installer, Package};
        use std::sync::Arc;

        let root = std::env::temp_dir().join(format!(
            "packlift-binary-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&root).unwrap();
        let app_dir = root.join("App-x64");
        fs::create_dir_all(&app_dir).unwrap();
        let bin_path = app_dir.join("app");
        fs::write(
            &bin_path,
            b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00>\x00",
        )
        .unwrap();

        let applications = root.join("applications");
        let installer = Installer::new(
            Arc::new(|_| {}),
            Some(root.join("data")),
            Some(applications.clone()),
        )
        .unwrap();
        let directory = installer.data.join("installed/app");
        let package = Package {
            path: bin_path,
            kind: "binary".into(),
            name: "App".into(),
            ..Package::default()
        };
        let record = installer
            .install_local_payload(&package, std::env::consts::ARCH, "app", &directory, false)
            .unwrap();

        assert!(record["exports"][0]["desktop"].is_string());
        assert!(applications.exists());
        assert_eq!(installer.registry.apps().unwrap().len(), 1);
        installer.uninstall(&record).unwrap();
        assert!(!directory.exists());
        assert!(installer.registry.apps().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }
}
