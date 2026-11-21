use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use xz2::read::XzDecoder;

#[derive(Debug, Error)]
pub enum PackliftError {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, PackliftError>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Package {
    pub path: PathBuf,
    pub kind: String,
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub requires: Vec<String>,
    pub identifier: String,
    pub description: String,
    pub checksum: String,
    pub signature: String,
    pub terminal: bool,
    pub digest: String,
    #[serde(skip)]
    pub icon: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub method: String,
    pub manager: String,
    pub family: String,
    pub multilib: bool,
}

pub fn architecture_group(value: &str) -> &str {
    let v = value.to_ascii_lowercase();
    match v.as_str() {
        "i386" | "i486" | "i586" | "i686" | "x86" | "win32" => "x86",
        "amd64" | "x86_64" | "win64" | "x64" => "x86_64",
        "all" | "noarch" | "neutral" | "any" | "" => "any",
        _ => value,
    }
}

pub fn elf_architecture(header: &[u8]) -> String {
    if header.len() < 20 || &header[..4] != b"\x7fELF" {
        return String::new();
    }
    let machine = if header[5] == 1 {
        u16::from_le_bytes([header[18], header[19]])
    } else {
        u16::from_be_bytes([header[18], header[19]])
    };
    match machine {
        3 => "x86".to_owned(),
        62 => "x86_64".to_owned(),
        183 => "arm64".to_owned(),
        40 => "arm".to_owned(),
        n => format!("ELF-{n}"),
    }
}

pub fn pe_architecture(path: &Path, header: &[u8]) -> Result<String> {
    if header.len() < 64 {
        return Err(PackliftError::Message(
            "This file does not contain a valid Windows PE header.".into(),
        ));
    }
    let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut pe = [0_u8; 6];
    file.read_exact(&mut pe).map_err(|_| {
        PackliftError::Message("This file does not contain a valid Windows PE header.".into())
    })?;
    if &pe[..4] != b"PE\0\0" {
        return Err(PackliftError::Message(
            "This file does not contain a valid Windows PE header.".into(),
        ));
    }
    let machine = u16::from_le_bytes([pe[4], pe[5]]);
    Ok(match machine {
        0x14c => "win32".into(),
        0x8664 => "win64".into(),
        0xaa64 => "arm64".into(),
        n => format!("PE-{n:04x}"),
    })
}

pub fn tar_header(header: &[u8]) -> bool {
    if header.len() < 512 {
        return false;
    }
    let stored = std::str::from_utf8(&header[148..156])
        .unwrap_or("")
        .trim_matches(['\0', ' ']);
    let Ok(checksum) = u64::from_str_radix(stored, 8) else {
        return false;
    };
    let sum: u64 = header[..148]
        .iter()
        .chain([b' '; 8].iter())
        .chain(header[156..512].iter())
        .map(|b| u64::from(*b))
        .sum();
    checksum > 0 && checksum == sum
}

pub fn detect(path: &Path) -> Result<(String, String)> {
    let mut file = File::open(path)?;
    let mut header = vec![0; 512];
    let count = file.read(&mut header)?;
    header.truncate(count);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let suffix = path
        .extension()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if header.starts_with(b"!<arch>\n")
        && header
            .get(8..24)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|b| *b != b'/' && *b != b' ')
            .collect::<Vec<_>>()
            == b"debian-binary"
    {
        return Ok(("deb".into(), String::new()));
    }
    if header.starts_with(b"\xed\xab\xee\xdb") {
        return Ok(("rpm".into(), String::new()));
    }
    if tar_header(&header) {
        return Ok(("tar".into(), String::new()));
    }
    if header.starts_with(b"\x7fELF")
        && header
            .get(8..11)
            .is_some_and(|v| v == b"AI\x01" || v == b"AI\x02")
    {
        return Ok(("appimage".into(), elf_architecture(&header)));
    }
    if header.starts_with(b"MZ") {
        return Ok((
            if suffix == "setup" { "setup" } else { "exe" }.into(),
            pe_architecture(path, &header)?,
        ));
    }
    if header.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        return Ok(("msi".into(), String::new()));
    }
    if header.starts_with(b"\x28\xb5\x2f\xfd") && name.ends_with(".pkg.tar.zst") {
        return Ok(("arch".into(), String::new()));
    }
    if header.starts_with(b"\x1f\x8b") {
        let mut decoded = [0_u8; 512];
        GzDecoder::new(File::open(path)?)
            .read_exact(&mut decoded)
            .map_err(|_| {
                PackliftError::Message("The file is not a supported tar archive.".into())
            })?;
        if tar_header(&decoded) {
            return Ok(("tar".into(), String::new()));
        }
    }
    if header.starts_with(b"\xfd7zXZ\0") {
        let mut decoded = [0_u8; 512];
        XzDecoder::new(File::open(path)?)
            .read_exact(&mut decoded)
            .map_err(|_| {
                PackliftError::Message("The file is not a supported tar archive.".into())
            })?;
        if tar_header(&decoded) {
            return Ok(("tar".into(), String::new()));
        }
    }
    Err(PackliftError::Message(
        "The file header does not match a supported package format.".into(),
    ))
}

pub fn host_info(release: &str, machine: &str, immutable: bool) -> (HashSet<String>, String) {
    let mut identifiers = HashSet::new();
    for line in release.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key == "ID" || key == "ID_LIKE" {
                identifiers.extend(
                    value
                        .trim_matches(['"', '\''])
                        .to_ascii_lowercase()
                        .split_whitespace()
                        .map(str::to_owned),
                );
            }
        }
    }
    if immutable {
        identifiers.insert("immutable".into());
    }
    (identifiers, machine.to_ascii_lowercase())
}

pub fn plan_for(package: &Package, host: &(HashSet<String>, String)) -> Result<Plan> {
    let (distro, host_machine) = host;
    let arch = architecture_group(&package.architecture);
    let machine = architecture_group(host_machine);
    if machine != "x86" && machine != "x86_64" {
        return Err(PackliftError::Message(
            "This host architecture is outside the supported x86 family.".into(),
        ));
    }
    if arch != "any" && arch != machine && !(machine == "x86_64" && arch == "x86") {
        return Err(PackliftError::Message(
            "The package architecture is incompatible with this computer.".into(),
        ));
    }
    let multilib = arch == "x86" && machine == "x86_64";
    let immutable = distro.contains("immutable");
    let plan = match package.kind.as_str() {
        "deb" => Plan {
            method: if !immutable && (distro.contains("debian") || distro.contains("ubuntu")) {
                "native"
            } else {
                "toolbox"
            }
            .into(),
            manager: "apt".into(),
            family: "ubuntu".into(),
            multilib,
        },
        "rpm" => {
            if machine == "x86" {
                return Err(PackliftError::Message(
                    "Current Fedora and openSUSE toolchains require a 64-bit host.".into(),
                ));
            }
            let suse = [
                "opensuse",
                "opensuse-leap",
                "opensuse-tumbleweed",
                "sles",
                "suse",
            ]
            .iter()
            .any(|id| distro.contains(*id));
            let fedora = ["fedora", "rhel", "centos", "rocky", "almalinux"]
                .iter()
                .any(|id| distro.contains(*id));
            Plan {
                method: if !immutable && suse || !immutable && fedora {
                    "native"
                } else {
                    "toolbox"
                }
                .into(),
                manager: if suse { "zypper" } else { "dnf" }.into(),
                family: if suse { String::new() } else { "fedora".into() },
                multilib,
            }
        }
        "arch" => {
            if machine == "x86" {
                return Err(PackliftError::Message(
                    "The supported Arch image requires a 64-bit host.".into(),
                ));
            }
            Plan {
                method: if distro.contains("arch") {
                    "native"
                } else {
                    "toolbox"
                }
                .into(),
                manager: "pacman".into(),
                family: "arch".into(),
                multilib: false,
            }
        }
        "exe" | "msi" | "setup" => Plan {
            method: "wine".into(),
            manager: String::new(),
            family: String::new(),
            multilib,
        },
        _ => Plan {
            method: "local".into(),
            manager: String::new(),
            family: String::new(),
            multilib,
        },
    };
    Ok(plan)
}

pub fn control_fields(text: &str, separator: char) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut last = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if line.len() != line.trim_start().len() && !last.is_empty() {
            map.entry(last.clone()).and_modify(|v: &mut String| {
                v.push(' ');
                v.push_str(line.trim());
            });
        } else if let Some((key, value)) = line.split_once(separator) {
            last = key.trim().to_owned();
            map.insert(last.clone(), value.trim().to_owned());
        }
    }
    map
}

pub fn environment(text: &str) -> Result<Vec<(String, String)>> {
    let valid = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("static regex");
    let reserved = [
        "WINEPREFIX",
        "WINEARCH",
        "WINE",
        "WINESERVER",
        "XDG_DATA_HOME",
        "STEAM_COMPAT_DATA_PATH",
        "STEAM_COMPAT_CLIENT_INSTALL_PATH",
        "GAMEID",
        "PROTONPATH",
        "UMU_FOLDERS_PATH",
    ];
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let Some((name, value)) = line.split_once('=') else {
                return Err(PackliftError::Message(
                    "Use one environment setting per line in NAME=value format.".into(),
                ));
            };
            if !valid.is_match(name) || reserved.contains(&name) || value.contains('\0') {
                return Err(PackliftError::Message(
                    "Use one environment setting per line in NAME=value format.".into(),
                ));
            }
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect()
}
