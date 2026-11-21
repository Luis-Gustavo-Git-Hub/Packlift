use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Arc,
};

use fs2::FileExt;
use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::{PackliftError, Result};

pub type LogHandler = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct Runner {
    pub data: PathBuf,
    pub sandbox: bool,
    log_path: PathBuf,
    output: LogHandler,
}

impl Runner {
    pub fn new(data: Option<PathBuf>, output: LogHandler) -> Result<Self> {
        let data = data.unwrap_or_else(|| data_home().join("flatapk"));
        fs::create_dir_all(&data)?;
        let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o700));
        Ok(Self {
            log_path: data.join("flatapk.log"),
            data,
            sandbox: Path::new("/.flatpak-info").exists(),
            output,
        })
    }

    fn command(&self, args: &[String], local: bool, watch_bus: bool) -> Vec<String> {
        if self.sandbox && args.first().is_some_and(|arg| arg == "flatpak-spawn") {
            return args.to_vec();
        }
        if self.sandbox && !local {
            let mut command = vec!["flatpak-spawn".into(), "--host".into()];
            if watch_bus {
                command.push("--watch-bus".into());
            }
            command.push(format!("--directory={}", home_dir().display()));
            command.extend(args.iter().cloned());
            command
        } else {
            args.to_vec()
        }
    }

    pub fn run(
        &self,
        args: &[String],
        local: bool,
        watch_bus: bool,
        check: bool,
    ) -> Result<Output> {
        let command = self.command(args, local, watch_bus);
        let shell = command
            .iter()
            .map(|v| shell_quote(v))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                PackliftError::Message(format!(
                    "Required command is missing: {} ({e}).",
                    command[0]
                ))
            })?;
        let stderr = child.stderr.take();
        let stderr_runner = self.clone();
        let stderr_reader = std::thread::spawn(move || {
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    stderr_runner.write(&line);
                }
            }
        });
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines() {
                let line = line?;
                self.write(&line);
            }
        }
        let output = child.wait_with_output()?;
        let _ = stderr_reader.join();
        if check && !output.status.success() {
            return Err(PackliftError::Message(format!(
                "{} exited with status {}. See the installation log.",
                args.first().map(String::as_str).unwrap_or("Command"),
                output.status.code().unwrap_or(1)
            )));
        }
        Ok(output)
    }

    pub fn start(&self, args: &[String]) -> Result<()> {
        let command = self.command(args, false, true);
        let shell = command
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                PackliftError::Message(format!(
                    "Could not start {}: {error}",
                    args.first().map(String::as_str).unwrap_or("program")
                ))
            })?;
        if let Some(stderr) = child.stderr.take() {
            let runner = self.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    runner.write(&line);
                }
            });
        }
        // Catch launchers that exit immediately with an error so the UI can
        // keep the log available instead of treating the game as launched.
        let launch_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait()? {
                self.write(&format!("Program exited during startup with status {status}"));
                if !status.success() {
                    return Err(PackliftError::Message(format!(
                        "The program failed to start (exit status {status}). See the installation log."
                    )));
                }
                return Ok(());
            }
            if std::time::Instant::now() >= launch_deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let runner = self.clone();
        std::thread::spawn(move || match child.wait() {
            Ok(status) => runner.write(&format!("Program exited with status {status}")),
            Err(error) => runner.write(&format!("Could not observe program exit: {error}")),
        });
        Ok(())
    }

    pub fn capture(&self, args: &[String], local: bool) -> Result<String> {
        let command = self.command(args, local, true);
        let shell = command
            .iter()
            .map(|v| shell_quote(v))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let output = Command::new(&command[0]).args(&command[1..]).output()?;
        if !output.status.success() {
            return Err(PackliftError::Message(format!("{} failed.", command[0])));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn capture_bytes(&self, args: &[String], local: bool) -> Result<Vec<u8>> {
        let command = self.command(args, local, true);
        let shell = command
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let output = Command::new(&command[0]).args(&command[1..]).output()?;
        if !output.status.success() {
            return Err(PackliftError::Message(format!("{} failed.", command[0])));
        }
        if output.stdout.len() > 2 * 1024 * 1024 {
            return Err(PackliftError::Message(
                "The application icon is too large to export safely.".into(),
            ));
        }
        Ok(output.stdout)
    }

    pub fn available(&self, executable: &str) -> bool {
        self.run(
            &[
                "/bin/sh".into(),
                "-c".into(),
                "command -v -- \"$1\"".into(),
                "packlift".into(),
                executable.into(),
            ],
            false,
            true,
            false,
        )
        .is_ok_and(|output| output.status.success())
    }

    pub fn write(&self, text: &str) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{text}");
        }
        (self.output)(text);
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

pub fn data_home() -> PathBuf {
    if Path::new("/.flatpak-info").exists() {
        if let Some(path) = std::env::var_os("HOST_XDG_DATA_HOME") {
            return PathBuf::from(path);
        }
    } else if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(path);
    }
    home_dir().join(".local/share")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn chrono_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| format!("{}", duration.as_secs()))
        .unwrap_or_default()
}

pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@=+,%~-".contains(c))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[derive(Clone)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn new(data: &Path) -> Result<Self> {
        fs::create_dir_all(data)?;
        let registry = Self {
            path: data.join("apps.sqlite"),
        };
        registry.connect()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS apps (id TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS containers (name TEXT PRIMARY KEY);",
        )?;
        Ok(registry)
    }

    fn connect(&self) -> Result<Connection> {
        Ok(Connection::open(&self.path)?)
    }

    pub fn apps(&self) -> Result<Vec<Value>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare("SELECT record FROM apps ORDER BY id")?;
        let values = statement.query_map([], |row| row.get::<_, String>(0))?;
        values
            .map(|value| Ok(serde_json::from_str(&value?)?))
            .collect()
    }

    pub fn save(&self, app: &Value) -> Result<()> {
        let id = app
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PackliftError::Message("The application record has no ID.".into()))?;
        self.connect()?.execute(
            "INSERT INTO apps VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
            params![id, serde_json::to_string(app)?],
        )?;
        Ok(())
    }

    pub fn remove(&self, app: &Value) -> Result<()> {
        if let Some(id) = app.get("id").and_then(Value::as_str) {
            self.connect()?
                .execute("DELETE FROM apps WHERE id = ?1", params![id])?;
        }
        Ok(())
    }

    pub fn containers(&self) -> Result<Vec<String>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare("SELECT name FROM containers ORDER BY name")?;
        let values = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(values)
    }

    pub fn add_container(&self, name: &str) -> Result<()> {
        self.connect()?.execute(
            "INSERT OR IGNORE INTO containers VALUES (?1)",
            params![name],
        )?;
        Ok(())
    }

    pub fn remove_container(&self, name: &str) -> Result<()> {
        self.connect()?
            .execute("DELETE FROM containers WHERE name = ?1", params![name])?;
        Ok(())
    }
}

pub fn operation(data: &Path) -> Result<File> {
    fs::create_dir_all(data)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(data.join("operation.lock"))?;
    file.try_lock_exclusive().map_err(|_| {
        PackliftError::Message(
            "Another Packlift operation is running. Try again when it finishes.".into(),
        )
    })?;
    Ok(file)
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn remove_exports(app: &Value) {
    if let Some(exports) = app.get("exports").and_then(Value::as_array) {
        for entry in exports {
            if let Some(path) = entry.get("desktop").and_then(Value::as_str) {
                let _ = fs::remove_file(path);
            }
        }
    }
    if let Some(path) = app.get("directory").and_then(Value::as_str) {
        let _ = fs::remove_dir_all(path);
    }
}

pub fn export_desktop(
    path: &Path,
    name: &str,
    argv: &[String],
    icon: &str,
    terminal: bool,
    category: &str,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| PackliftError::Message("Invalid desktop shortcut path.".into()))?;
    fs::create_dir_all(parent)?;
    let exec = argv
        .iter()
        .map(|s| desktop_exec_arg(s))
        .collect::<Vec<_>>()
        .join(" ");
    let data = format!("[Desktop Entry]\nType=Application\nName={}\nExec={}\nIcon={}\nTerminal={}\nCategories={};\nX-Flatapk-Managed=true\n",
        desktop_string(name), exec, desktop_string(icon), if terminal { "true" } else { "false" }, desktop_string(category));
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, data)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn desktop_string(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

fn desktop_exec_arg(value: &str) -> String {
    let value = value.replace('%', "%%");
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@=+,%~-".contains(c))
    {
        value
    } else {
        let mut escaped = String::from("\"");
        for character in value.chars() {
            if ['"', '\\', '$', '`'].contains(&character) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        escaped.push('"');
        escaped
    }
}
