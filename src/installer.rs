use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{
    core::{architecture_group, detect, Package, PackliftError, Result},
    system::{export_desktop, operation, remove_exports, sha256, Registry, Runner},
};

const FLATPAK_WINE: &str = "org.winehq.Wine//wow64-25.08";

#[derive(Clone)]
pub struct Installer {
    pub data: PathBuf,
    pub applications: PathBuf,
    pub runner: Runner,
    pub registry: Registry,
}

impl Installer {
    pub fn new(
        output: crate::system::LogHandler,
        data: Option<PathBuf>,
        applications: Option<PathBuf>,
    ) -> Result<Self> {
        let data = data.unwrap_or_else(|| crate::system::data_home().join("flatapk"));
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
        installer.refresh_game_menu_icons();
        Ok(installer)
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
        self.runner
            .write(&format!("Analyzing package: {}", path.display()));
        let (kind, architecture) = detect(path)?;
        let mut package = Package {
            path: path.to_owned(),
            kind: kind.clone(),
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Program")
                .to_owned(),
            architecture,
            signature: "unknown".into(),
            ..Default::default()
        };
        package.digest = sha256(path)?;
        match kind.as_str() {
            "deb" => self.inspect_deb(&mut package)?,
            "rpm" => self.inspect_rpm(&mut package)?,
            "arch" => self.inspect_arch(&mut package)?,
            "msi" => self.inspect_msi(&mut package)?,
            "exe" | "setup" => self.inspect_pe_icon(&mut package)?,
            "appimage" => self.inspect_appimage(&mut package)?,
            "tar" => self.inspect_archive(&mut package)?,
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
        let release = self
            .runner
            .capture(&["cat".into(), "/etc/os-release".into()], false)
            .unwrap_or_default();
        let machine = if self.runner.sandbox {
            self.runner.capture(
                &[
                    "flatpak-spawn".into(),
                    "--host".into(),
                    "uname".into(),
                    "-m".into(),
                ],
                true,
            )?
        } else {
            std::env::consts::ARCH.to_owned()
        };
        let host = crate::core::host_info(
            &release,
            machine.trim(),
            Path::new("/run/ostree-booted").exists(),
        );
        let plan = crate::core::plan_for(&package, &host)?;
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
                if output.lines().any(|line| {
                    line.to_ascii_lowercase().contains("signature")
                        && line.to_ascii_lowercase().contains("ok")
                }) {
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
                false,
            )?;
            package.signature = "verified".into();
        }
        Ok(())
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
                .trim_end_matches(".tar")
                .to_owned();
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
                    source.clone(),
                    desktop.clone(),
                ],
                true,
            );
            let Ok(output) = output else { continue };
            let mut in_entry = false;
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
                    }
                } else if let Some(value) = line.strip_prefix("Icon=") {
                    requested_icon = value.trim().to_owned();
                } else if line == "Terminal=true" {
                    package.terminal = true;
                }
            }
            if !requested_icon.is_empty()
                || package.name
                    != package
                        .path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Program")
            {
                break;
            }
        }
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

    fn inspect_pe_icon(&self, package: &mut Package) -> Result<()> {
        if let Some(bytes) = pe_icon(&package.path)? {
            let directory = self.data.join("package-icons");
            fs::create_dir_all(&directory)?;
            let (suffix, bytes) = png_payload_from_ico(&bytes)
                .map(|png| ("png", png.to_vec()))
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
        let machine = self.machine()?;
        let release = self
            .runner
            .capture(&["cat".into(), "/etc/os-release".into()], false)
            .unwrap_or_default();
        let host =
            crate::core::host_info(&release, &machine, Path::new("/run/ostree-booted").exists());
        let plan = crate::core::plan_for(package, &host)?;
        match plan.method.as_str() {
            "wine" => self.install_windows_program(package, runtime, &machine),
            "local" => self.install_local(package, &machine),
            "native" | "toolbox" => self.install_system_package(package, &plan),
            _ => Err(PackliftError::Message(
                "Unsupported package installation plan.".into(),
            )),
        }
    }

    fn install_system_package(&self, package: &Package, plan: &crate::core::Plan) -> Result<Value> {
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
        if already_installed(&self.runner, &plan.manager, &target, &container)? {
            return Err(PackliftError::Message(
                "This system package is already installed.".into(),
            ));
        }
        if plan.multilib && plan.manager == "apt" {
            let foreign = package_capture(
                &self.runner,
                &container,
                &["dpkg".into(), "--print-foreign-architectures".into()],
            )?;
            if !foreign.split_whitespace().any(|value| value == "i386") {
                elevated(
                    &self.runner,
                    &container,
                    &["dpkg".into(), "--add-architecture".into(), "i386".into()],
                )?;
            }
        }
        let source = package.path.to_string_lossy().into_owned();
        self.runner.write("Downloading package dependencies");
        if plan.manager == "apt" {
            elevated(
                &self.runner,
                &container,
                &[
                    "apt-get".into(),
                    "install".into(),
                    "--no-remove".into(),
                    "--download-only".into(),
                    "-y".into(),
                    source.clone(),
                ],
            )?;
        } else if plan.manager == "dnf" {
            elevated(
                &self.runner,
                &container,
                &[
                    "dnf".into(),
                    "install".into(),
                    "--downloadonly".into(),
                    "-y".into(),
                    source.clone(),
                ],
            )?;
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
        command.push(source);
        self.runner.write("Installing the system package");
        elevated(&self.runner, &container, &command)?;
        let identifier = short_hash(&format!("{}:{}", package.kind, package.digest))?;
        let directory = self.data.join("installed").join(&identifier);
        fs::create_dir_all(&directory)?;
        let mut launchers =
            system_launchers(&self.runner, package, &plan.manager, &target, &container)?;
        if !container.is_empty() {
            for (index, (_, _, icon, _)) in launchers.iter_mut().enumerate() {
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
        Ok(app)
    }

    fn ensure_toolbox(&self, family: &str) -> Result<String> {
        self.runner
            .write(&format!("Preparing the {family} Toolbox environment"));
        if !self.runner.available("toolbox") || !self.runner.available("podman") {
            return Err(PackliftError::Message(
                "Install Toolbox and Podman on the host before continuing.".into(),
            ));
        }
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
        let image = std::env::var(format!("FLATAPK_{}_IMAGE", family.to_ascii_uppercase()))
            .unwrap_or_else(|_| match family {
                "ubuntu" => "quay.io/toolbx/ubuntu-toolbox:24.04".into(),
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
        launchers: &[(String, Vec<String>, String, bool)],
        category: &str,
    ) -> Result<Vec<Value>> {
        let mut exports = Vec::new();
        for (index, (name, argv, icon, terminal)) in launchers.iter().enumerate() {
            let path = self
                .applications
                .join(format!("io.github.flatapk.Installer.{id}.{index}.desktop"));
            let displayed_icon = if icon.is_empty() {
                "application-x-executable"
            } else {
                icon
            };
            export_desktop(&path, name, argv, displayed_icon, *terminal, category)?;
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
        let (argv, terminal) = if package.kind == "appimage" {
            let target = directory.join("program.AppImage");
            fs::copy(&package.path, &target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
            (
                vec![
                    target.to_string_lossy().into_owned(),
                    "--appimage-extract-and-run".into(),
                ],
                package.terminal,
            )
        } else if package.kind == "tar" {
            let payload = directory.join("payload");
            fs::create_dir_all(&payload)?;
            let targets = unpack_archive(&package.path, &payload)?;
            let host_arch = architecture_group(machine);
            let target = targets
                .into_iter()
                .find(|target| {
                    let Ok(mut file) = fs::File::open(target) else {
                        return false;
                    };
                    let mut header = [0; 20];
                    let Ok(count) = std::io::Read::read(&mut file, &mut header) else {
                        return false;
                    };
                    let arch = crate::core::elf_architecture(&header[..count]);
                    arch == host_arch || (host_arch == "x86_64" && arch == "x86")
                })
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
        } else {
            return Err(PackliftError::Message(
                "This local package format is not available in the Rust build yet.".into(),
            ));
        };
        let export = self.applications.join(format!(
            "io.github.flatapk.Installer.{identifier}.0.desktop"
        ));
        let icon = package
            .icon
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "application-x-executable".into());
        export_desktop(&export, &package.name, &argv, &icon, terminal, "Utility")?;
        self.runner.write("Creating the application menu shortcut");
        let app = json!({"id":identifier,"name":package.name,"version":package.version,"architecture":package.architecture,
            "method":"local","source":package.path,"digest":package.digest,"directory":directory,
            "exports":[{"desktop":export,"name":package.name,"icon":icon,"argv":argv,"terminal":terminal}]});
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
            .write("Registering the installed program in Packlift");
        let prefix = self.data.join("wine/prefix/drive_c");
        let mut executables = Vec::new();
        collect_exes(&prefix.join("Program Files"), &mut executables, 0)?;
        collect_exes(&prefix.join("Program Files (x86)"), &mut executables, 0)?;
        let needle = package.name.to_ascii_lowercase();
        executables.sort_by_key(|path| {
            (
                path.file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    != needle,
                path.components().count(),
                path.clone(),
            )
        });
        let target = executables.into_iter().next().ok_or_else(|| PackliftError::Message(
            "The installer finished, but Packlift could not find the installed program. Add its game executable from the file manager.".into()))?;
        let windows_directory = windows_install_directory(&target, &prefix);
        let export = self.applications.join(format!(
            "io.github.flatapk.Installer.{identifier}.0.desktop"
        ));
        let argv = wine.command(
            "wine",
            &[target.to_string_lossy().into_owned()],
            Some(target.parent().unwrap_or(&prefix)),
        );
        let icon = package
            .icon
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "application-x-executable".into());
        let (menu_icon, menu_icon_file) =
            self.export_menu_icon(&identifier, package.icon.as_deref())?;
        export_desktop(&export, &package.name, &argv, &menu_icon, false, "Utility")?;
        let metadata_directory = self.data.join("windows").join(&identifier);
        fs::create_dir_all(&metadata_directory)?;
        let app = json!({"id":identifier,"name":package.name,"version":package.version,"architecture":package.architecture,
            "method":"wine","source":package.path,"digest":package.digest,"directory":metadata_directory,
            "installer":package.path,"runtime":wine.runtime,"windows_directory":windows_directory,"exports":[{"desktop":export,"name":package.name,
            "icon":icon,"menu_icon_file":menu_icon_file,"argv":argv,"terminal":false,"wine_args":[target.to_string_lossy()]}]});
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
        let identifier = format!(
            "{}",
            &sha256_bytes(format!("game:{}", package.path.display()).as_bytes())?[..20]
        );
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
        let export_path = self.applications.join(format!(
            "io.github.flatapk.Installer.{identifier}.0.desktop"
        ));
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
        let (menu_icon, menu_icon_file) =
            self.export_menu_icon(&identifier, package.icon.as_deref())?;
        export_desktop(
            &export_path,
            &package.name,
            &argv,
            &menu_icon,
            false,
            "Game",
        )?;
        self.runner.write("Creating the game menu shortcut");
        let app = json!({
            "id": identifier, "name": package.name, "version": package.version,
            "architecture": package.architecture, "source": package.path,
            "digest": digest, "directory": identifier_path, "exports": [{
                "desktop": export_path, "name": package.name, "icon": icon,
                "menu_icon_file": menu_icon_file,
                "argv": argv, "terminal": false, "wine_args": [package.path.to_string_lossy()]
            }], "method": "wine", "runtime": wine.runtime,
            "game_shortcut": true, "working_directory": app_dir
        });
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
        let Some(extension) = source
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| {
                ["png", "ico", "svg", "xpm"]
                    .iter()
                    .any(|supported| extension.eq_ignore_ascii_case(supported))
            })
        else {
            return Ok(("application-x-executable".into(), None));
        };
        let directory = self.applications.join("icons");
        fs::create_dir_all(&directory)?;
        let icon = directory.join(format!("packlift-{identifier}.{extension}"));
        fs::copy(source, &icon)?;
        let path = icon.to_string_lossy().into_owned();
        Ok((path.clone(), Some(path)))
    }

    fn refresh_game_menu_icons(&self) {
        let Ok(apps) = self.registry.apps() else {
            return;
        };
        for mut app in apps {
            if !app["game_shortcut"].as_bool().unwrap_or(false) {
                continue;
            }
            let Some(icon) = app["exports"][0]["icon"].as_str().map(str::to_owned) else {
                continue;
            };
            let source = Path::new(&icon);
            if !source.is_file() {
                continue;
            }
            if app["exports"][0]["menu_icon_file"]
                .as_str()
                .is_some_and(|path| Path::new(path).is_file())
            {
                continue;
            }
            let Some(identifier) = app["id"].as_str().map(str::to_owned) else {
                continue;
            };
            let Ok((menu_icon, menu_icon_file)) =
                self.export_menu_icon(&identifier, Some(source))
            else {
                continue;
            };
            let (Some(desktop), Some(icon_file), Some(name), Some(arguments)) = (
                app["exports"][0]["desktop"].as_str().map(str::to_owned),
                menu_icon_file,
                app["exports"][0]["name"].as_str().map(str::to_owned),
                app["exports"][0]["argv"].as_array().cloned(),
            ) else {
                continue;
            };
            let argv = arguments
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let terminal = app["exports"][0]["terminal"].as_bool().unwrap_or(false);
            if export_desktop(
                Path::new(&desktop),
                &name,
                &argv,
                &menu_icon,
                terminal,
                "Game",
            )
            .is_ok()
            {
                app["exports"][0]["menu_icon_file"] = json!(icon_file);
                let _ = self.registry.save(&app);
            }
        }
    }

    pub fn install_windows(&self, package: &Package, runtime: &str) -> Result<Value> {
        let machine = self.machine()?;
        self.install_windows_program(package, runtime, &machine)
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
        self.runner.start(&command)
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
        remove_exports(app);
        self.remove_managed_directory(app, "games")?;
        self.remove_unreferenced_icons(app)?;
        self.registry.remove(app)?;
        Ok(())
    }

    pub fn uninstall(&self, app: &Value) -> Result<()> {
        let _operation = operation(&self.data)?;
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
        remove_exports(app);
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
        if directory.starts_with(expected) && directory.is_dir() {
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

fn read_rpm_header(
    file: &mut fs::File,
) -> Result<(std::collections::HashMap<u32, (u32, u32, u32)>, Vec<u8>)> {
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

struct Wine {
    runner: Runner,
    runtime: String,
    prefix: PathBuf,
    integrations: PathBuf,
    umu: bool,
    flatpak: bool,
}

impl Wine {
    fn new(
        runner: Runner,
        runtime: String,
        data: &Path,
        _applications: &Path,
        _machine: &str,
    ) -> Self {
        let root = data.join("wine");
        Self {
            runner,
            umu: runtime == "proton",
            flatpak: runtime == FLATPAK_WINE,
            runtime,
            prefix: root.join("prefix"),
            integrations: root.join("desktop"),
        }
    }

    fn command(&self, tool: &str, args: &[String], cwd: Option<&Path>) -> Vec<String> {
        let mut values = vec![("WINEPREFIX", self.prefix.to_string_lossy().to_string())];
        if self.umu {
            values.push(("GAMEID", "umu-default".into()));
            values.push((
                "UMU_FOLDERS_PATH",
                self.runner.data.to_string_lossy().to_string(),
            ));
        }
        values.push((
            "XDG_DATA_HOME",
            self.integrations.to_string_lossy().to_string(),
        ));
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

fn appimage_offset(path: &Path) -> Result<Option<u64>> {
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
    if optional_size < 120 || optional_size > 1024 {
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
    if resource_size < 16 || resource_size > MAX_RESOURCE_BYTES {
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
        Ok(!status
            .split_whitespace()
            .last()
            .is_none_or(|v| v == "not-installed" || v == "config-files"))
    } else {
        Ok(true)
    }
}

fn system_launchers(
    runner: &Runner,
    package: &Package,
    manager: &str,
    target: &str,
    container: &str,
) -> Result<Vec<(String, Vec<String>, String, bool)>> {
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
        let candidate = files
            .iter()
            .find(|p| {
                Path::new(p)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .is_some_and(|filename| {
                        filename
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
        ));
    }
    if launchers.is_empty() {
        let executable = files
            .iter()
            .find(|path| {
                ["/usr/bin", "/usr/local/bin", "/bin"].contains(
                    &Path::new(path)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or(""),
                )
            })
            .ok_or_else(|| {
                PackliftError::Message(
                    "The package did not provide an application launcher or executable.".into(),
                )
            })?;
        let mut argv = package_prefix(container);
        argv.push(executable.clone());
        launchers.push((package.name.clone(), argv, String::new(), true));
    }
    Ok(launchers)
}

fn unpack_archive(path: &Path, destination: &Path) -> Result<Vec<PathBuf>> {
    let mut probe = fs::File::open(path)?;
    let mut magic = [0_u8; 6];
    let count = std::io::Read::read(&mut probe, &mut magic)?;
    drop(probe);
    let input: Box<dyn std::io::Read> = if magic[..count].starts_with(b"\x1f\x8b") {
        Box::new(flate2::read::GzDecoder::new(fs::File::open(path)?))
    } else if magic[..count].starts_with(b"\xfd7zXZ\0") {
        Box::new(xz2::read::XzDecoder::new(fs::File::open(path)?))
    } else {
        Box::new(fs::File::open(path)?)
    };
    let mut archive = tar::Archive::new(input);
    let mut executable_files = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
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
        let target = destination.join(relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if !kind.is_file() {
            return Err(PackliftError::Message(
                "The archive contains a link or special file. Use an AppImage or a system package."
                    .into(),
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
    Ok(executable_files)
}

fn collect_exes(directory: &Path, output: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 10 || !directory.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            collect_exes(&path, output, depth + 1)?;
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            output.push(path);
        }
        if output.len() > 2048 {
            return Err(PackliftError::Message(
                "The installer created too many executable files to inspect.".into(),
            ));
        }
    }
    Ok(())
}
