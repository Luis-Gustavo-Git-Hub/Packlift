use crate::{
    core::{PackliftError, Result},
    system::{host_access_available, operation, Runner},
};

pub(crate) fn installation_flag(scope: &str) -> Option<String> {
    match scope {
        "user" => Some("--user".into()),
        "system" => Some("--system".into()),
        scope => scope
            .strip_prefix("system (")
            .and_then(|name| name.strip_suffix(')'))
            .filter(|name| !name.is_empty() && !name.contains(char::is_control))
            .map(|name| format!("--installation={name}")),
    }
}

#[derive(Clone, Copy)]
pub enum Access {
    Share,
    Socket,
    Device,
    Feature,
}

impl Access {
    pub fn enabled(self, permissions: &str, name: &str) -> bool {
        let key = match self {
            Self::Share => "shared",
            Self::Socket => "sockets",
            Self::Device => "devices",
            Self::Feature => "features",
        };
        permissions
            .lines()
            .skip_while(|line| *line != "[Context]")
            .skip(1)
            .take_while(|line| !line.starts_with('['))
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .is_some_and(|values| values.split(';').any(|value| value == name))
    }

    fn flag(self, enabled: bool) -> &'static str {
        match (self, enabled) {
            (Self::Share, true) => "share",
            (Self::Share, false) => "unshare",
            (Self::Socket, true) => "socket",
            (Self::Socket, false) => "nosocket",
            (Self::Device, true) => "device",
            (Self::Device, false) => "nodevice",
            (Self::Feature, true) => "allow",
            (Self::Feature, false) => "disallow",
        }
    }
}

pub enum FileAccess {
    ReadWrite,
    ReadOnly,
    Create,
    Deny,
}

pub enum BusAccess {
    Talk,
    Own,
    Deny,
}

pub enum Change {
    Toggle(Access, &'static str, bool),
    Filesystem(String, FileAccess),
    Bus {
        system: bool,
        name: String,
        level: BusAccess,
    },
    Environment {
        name: String,
        value: Option<String>,
    },
}

impl Change {
    fn argument(&self) -> Result<String> {
        let invalid = |message: &str| PackliftError::Message(message.into());
        Ok(match self {
            Self::Toggle(kind, name, enabled) => format!("--{}={name}", kind.flag(*enabled)),
            Self::Filesystem(path, access) => {
                if path.is_empty() || path.contains([':', ';']) || path.contains(char::is_control) {
                    return Err(invalid(
                        "Enter a Flatpak filesystem path without an access suffix.",
                    ));
                }
                match access {
                    FileAccess::ReadWrite => format!("--filesystem={path}"),
                    FileAccess::ReadOnly => format!("--filesystem={path}:ro"),
                    FileAccess::Create => format!("--filesystem={path}:create"),
                    FileAccess::Deny => format!("--nofilesystem={path}"),
                }
            }
            Self::Bus {
                system,
                name,
                level,
            } => {
                let service = name.strip_suffix(".*").unwrap_or(name);
                if !service.contains('.')
                    || !service
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                {
                    return Err(invalid(
                        "Enter a D-Bus service name such as org.example.Service.",
                    ));
                }
                let scope = if *system { "system-" } else { "" };
                let policy = match level {
                    BusAccess::Talk => "talk",
                    BusAccess::Own => "own",
                    BusAccess::Deny => "no-talk",
                };
                format!("--{scope}{policy}-name={name}")
            }
            Self::Environment { name, value } => {
                if name.is_empty()
                    || !name.bytes().enumerate().all(|(index, byte)| {
                        byte == b'_'
                            || byte.is_ascii_alphabetic()
                            || (index > 0 && byte.is_ascii_digit())
                    })
                    || value.as_ref().is_some_and(|value| value.contains('\0'))
                {
                    return Err(invalid("Enter a valid environment variable name."));
                }
                match value {
                    Some(value) => format!("--env={name}={value}"),
                    None => format!("--unset-env={name}"),
                }
            }
        })
    }
}

pub struct Permissions {
    pub effective: String,
    pub portal: Option<String>,
}

pub struct FlatpakTarget {
    pub reference: String,
    pub installation: String,
}

impl FlatpakTarget {
    fn app_id(&self) -> Result<&str> {
        let id = self.reference.split('/').next().unwrap_or_default();
        if !host_access_available() {
            return Err(PackliftError::HostAccessRequired);
        }
        if id.split('.').count() < 3
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(PackliftError::Message(
                "Invalid Flatpak application ID.".into(),
            ));
        }
        Ok(id)
    }

    pub fn permissions(&self, runner: &Runner) -> Result<Permissions> {
        let id = self.app_id()?;
        let installation = installation_flag(&self.installation)
            .ok_or_else(|| PackliftError::Message("Unknown Flatpak installation.".into()))?;
        let effective = runner.capture(
            &[
                "flatpak".into(),
                "info".into(),
                installation,
                "--show-permissions".into(),
                "--".into(),
                format!("app/{}", self.reference),
            ],
            false,
        )?;
        let portal = runner
            .capture(
                &[
                    "flatpak".into(),
                    "permission-show".into(),
                    "--".into(),
                    id.into(),
                ],
                false,
            )
            .ok();
        Ok(Permissions { effective, portal })
    }

    pub fn apply(&self, runner: &Runner, changes: &[Change]) -> Result<()> {
        let id = self.app_id()?;
        if changes.is_empty() {
            return Ok(());
        }
        let _operation = operation(&runner.data)?;
        let mut args = vec!["flatpak".into(), "override".into(), "--user".into()];
        for change in changes {
            args.push(change.argument()?);
        }
        args.extend(["--".into(), id.into()]);
        let private = args
            .iter()
            .any(|arg| arg.starts_with("--env="))
            .then_some("--env=");
        runner.capture_with_private_prefix(&args, false, private)?;
        Ok(())
    }

    pub fn reset(&self, runner: &Runner, portal: bool) -> Result<()> {
        let id = self.app_id()?;
        let _operation = operation(&runner.data)?;
        let mut args = if portal {
            vec!["flatpak".into(), "permission-reset".into()]
        } else {
            vec![
                "flatpak".into(),
                "override".into(),
                "--user".into(),
                "--reset".into(),
            ]
        };
        args.extend(["--".into(), id.into()]);
        runner.capture(&args, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_use_specific_flatpak_override_flags() {
        assert_eq!(
            Change::Toggle(Access::Socket, "x11", false)
                .argument()
                .unwrap(),
            "--nosocket=x11"
        );
        assert_eq!(
            Change::Filesystem("~/My Games".into(), FileAccess::ReadOnly)
                .argument()
                .unwrap(),
            "--filesystem=~/My Games:ro"
        );
        assert_eq!(
            Change::Bus {
                system: true,
                name: "org.example.Service".into(),
                level: BusAccess::Deny
            }
            .argument()
            .unwrap(),
            "--system-no-talk-name=org.example.Service"
        );
        assert_eq!(
            Change::Environment {
                name: "MY_VAR".into(),
                value: Some("a=b".into())
            }
            .argument()
            .unwrap(),
            "--env=MY_VAR=a=b"
        );
        assert!(Change::Environment {
            name: "X=Y".into(),
            value: None
        }
        .argument()
        .is_err());
        assert!(Change::Filesystem("home:ro".into(), FileAccess::ReadOnly)
            .argument()
            .is_err());
        assert_eq!(
            installation_flag("system (games)").as_deref(),
            Some("--installation=games")
        );
    }

    #[test]
    fn active_toggles_ignore_denials_and_other_sections() {
        let input = "[Context]\nshared=network;\nsockets=!x11;wayland;\n[Other]\nsockets=x11;\n";
        assert!(Access::Share.enabled(input, "network"));
        assert!(Access::Socket.enabled(input, "wayland"));
        assert!(!Access::Socket.enabled(input, "x11"));
        assert!(!Access::Device.enabled(input, "dri"));
    }
}
