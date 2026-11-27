use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock,
    },
    time::{Duration, Instant},
};

use fs2::FileExt;
use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::{PackliftError, Result};

pub type LogHandler = Arc<dyn Fn(&str) + Send + Sync>;

fn kill_process_group(child: &mut std::process::Child) {
    let process_group = -(child.id() as i32);
    // Commands are launched in their own process group so cancellation also
    // stops package-manager helpers and network clients they started.
    let killed = unsafe { libc::kill(process_group, libc::SIGKILL) } == 0;
    if !killed {
        let _ = child.kill();
    }
}

thread_local! {
    static CURRENT_CANCELLATION: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

pub fn with_cancellation_token<T>(token: Arc<AtomicBool>, task: impl FnOnce() -> T) -> T {
    let previous = CURRENT_CANCELLATION.with(|current| current.replace(Some(token)));
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT_CANCELLATION.with(|current| current.replace(previous));
        }
    }
    let _restore = Restore(previous);
    task()
}

fn current_cancellation_token() -> Arc<AtomicBool> {
    CURRENT_CANCELLATION
        .with_borrow(|current| current.clone())
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
}

pub fn cancellation_requested() -> bool {
    CURRENT_CANCELLATION.with_borrow(|current| {
        current
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Relaxed))
    })
}

const LOG_LIMIT: u64 = 10 * 1024 * 1024;

struct LogFile {
    writer: BufWriter<File>,
    bytes: u64,
}

#[derive(Clone)]
pub struct Runner {
    pub data: PathBuf,
    pub sandbox: bool,
    log_path: PathBuf,
    log_file: Arc<Mutex<LogFile>>,
    available: Arc<Mutex<std::collections::HashMap<String, bool>>>,
    output: LogHandler,
    cancellation: Arc<AtomicBool>,
}

impl Runner {
    pub fn new(data: Option<PathBuf>, output: LogHandler) -> Result<Self> {
        let data = data.unwrap_or_else(packlift_data_dir);
        fs::create_dir_all(&data)?;
        let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o700));
        let log_path = data.join("flatapk.log");
        let log_file = open_log(&log_path)?;
        Ok(Self {
            log_path,
            log_file: Arc::new(Mutex::new(log_file)),
            available: Arc::new(Mutex::new(std::collections::HashMap::new())),
            data,
            sandbox: Path::new("/.flatpak-info").exists(),
            output,
            cancellation: current_cancellation_token(),
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
        let mut command = self.command(args, local, watch_bus);
        let flatpak_transaction = args.first().is_some_and(|arg| arg == "flatpak")
            && args
                .get(1)
                .is_some_and(|arg| matches!(arg.as_str(), "install" | "update" | "uninstall"));
        // The operation panel recognizes the CLI's untranslated progress messages.
        if flatpak_transaction && self.sandbox && !local {
            command.insert(2, "--env=LC_ALL=C.UTF-8".into());
        }
        let shell = command
            .iter()
            .map(|v| shell_quote(v))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let mut process = Command::new(&command[0]);
        process.args(&command[1..]);
        process.process_group(0);
        if flatpak_transaction {
            process.env("LC_ALL", "C.UTF-8");
        }
        let mut child = process
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
        let (stderr_tx, stderr_rx) = mpsc::channel();
        let _stderr_reader = std::thread::spawn(move || {
            let mut tail = std::collections::VecDeque::new();
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    stderr_runner.write(&line);
                    if tail.len() == 5 {
                        tail.pop_front();
                    }
                    tail.push_back(line.chars().take(500).collect::<String>());
                }
            }
            let _ = stderr_tx.send(tail.into_iter().collect::<Vec<_>>().join("\n"));
        });
        let (stdout_tx, stdout_rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) => {
                            if stdout_tx.send(Ok(line)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = stdout_tx.send(Err(error));
                            break;
                        }
                    }
                }
            });
        }
        let started = Instant::now();
        let timeout = Duration::from_secs(60 * 60);
        let mut timed_out = false;
        let mut canceled = false;
        loop {
            while let Ok(line) = stdout_rx.try_recv() {
                let line = line?;
                self.write(&line);
            }
            if child.try_wait()?.is_some() {
                break;
            }
            if self.cancellation.load(Ordering::Relaxed) {
                canceled = true;
                kill_process_group(&mut child);
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                kill_process_group(&mut child);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let status = child.wait()?;
        while let Ok(line) = stdout_rx.try_recv() {
            if let Ok(line) = line {
                self.write(&line);
            }
        }
        let diagnostic = stderr_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default();
        let output = Output {
            status,
            stdout: Vec::new(),
            stderr: diagnostic.as_bytes().to_vec(),
        };
        if canceled {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        if timed_out {
            return Err(PackliftError::Message(
                "The operation timed out after 60 minutes. See the installation log.".into(),
            ));
        }
        if check && !output.status.success() {
            return Err(PackliftError::Message(format!(
                "{} exited with status {}. See the installation log.\n{diagnostic}",
                args.first().map(String::as_str).unwrap_or("Command"),
                output.status.code().unwrap_or(1)
            )));
        }
        Ok(output)
    }

    pub fn start(&self, args: &[String]) -> Result<()> {
        self.start_with_scope(args, false, true)
    }

    pub fn start_in_sandbox(&self, args: &[String]) -> Result<()> {
        self.start_with_scope(args, true, true)
    }

    pub fn start_detached(&self, args: &[String]) -> Result<()> {
        self.start_with_scope(args, false, false)
    }

    fn start_with_scope(&self, args: &[String], local: bool, watch_bus: bool) -> Result<()> {
        let command = self.command(args, local, watch_bus);
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
                self.write(&format!(
                    "Program exited during startup with status {status}"
                ));
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
        self.capture_with_private_prefix(args, local, None)
    }

    pub(crate) fn capture_with_private_prefix(
        &self,
        args: &[String],
        local: bool,
        private_prefix: Option<&str>,
    ) -> Result<String> {
        let command = self.command(args, local, true);
        let shell = command
            .iter()
            .map(
                |value| match private_prefix.filter(|prefix| value.starts_with(prefix)) {
                    Some(prefix) => shell_quote(&format!("{prefix}[redacted]")),
                    None => shell_quote(value),
                },
            )
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let stdout = child.stdout.take().expect("piped capture stdout");
        let stderr = child.stderr.take().expect("piped capture stderr");
        let out_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = BufReader::new(stdout).read_to_end(&mut bytes);
            bytes
        });
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut bytes);
            bytes
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if self.cancellation.load(Ordering::Relaxed) {
                kill_process_group(&mut child);
                let _ = child.wait();
                return Err(PackliftError::Message("Operation canceled.".into()));
            }
            if started.elapsed() >= Duration::from_secs(60) {
                kill_process_group(&mut child);
                let _ = child.wait();
                return Err(PackliftError::Message(
                    "The command timed out after 60 seconds.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let output = Output {
            status,
            stdout: out_reader.join().unwrap_or_default(),
            stderr: err_reader.join().unwrap_or_default(),
        };
        if !output.status.success() {
            if private_prefix.is_some() {
                self.write(&format!("{} exited with {}", args[0], output.status));
                return Err(PackliftError::Message(format!(
                    "{} exited with {}.",
                    args[0], output.status
                )));
            }
            let detail: String = String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2500)
                .collect();
            self.write(&detail);
            return Err(PackliftError::Message(format!(
                "{} failed. {detail}",
                command[0]
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn capture_bytes(&self, args: &[String], local: bool) -> Result<Vec<u8>> {
        self.capture_bytes_limited(args, local, 2 * 1024 * 1024)
    }

    pub(crate) fn capture_bytes_limited(
        &self,
        args: &[String],
        local: bool,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let command = self.command(args, local, true);
        let shell = command
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("{} $ {shell}", chrono_timestamp()));
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let stdout = child.stdout.take().expect("piped command stdout");
        let stderr = child.stderr.take().expect("piped command stderr");
        let out_reader = std::thread::spawn(move || {
            let mut data = Vec::new();
            let mut total = 0_u64;
            let mut reader = BufReader::new(stdout);
            let mut buffer = [0_u8; 16 * 1024];
            let mut error = None;
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        total = total.saturating_add(count as u64);
                        if data.len() as u64 <= limit {
                            let keep = (limit.saturating_add(1).saturating_sub(data.len() as u64))
                                as usize;
                            data.extend_from_slice(&buffer[..count.min(keep)]);
                        }
                    }
                    Err(read_error) => {
                        error = Some(read_error);
                        break;
                    }
                }
            }
            (data, total > limit, error)
        });
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = BufReader::new(stderr)
                .take(64 * 1024)
                .read_to_end(&mut bytes);
            bytes
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if self.cancellation.load(Ordering::Relaxed) {
                kill_process_group(&mut child);
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(PackliftError::Message("Operation canceled.".into()));
            }
            if started.elapsed() >= Duration::from_secs(60) {
                kill_process_group(&mut child);
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(PackliftError::Message(
                    "The command timed out after 60 seconds.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let (data, too_large, read_error) = out_reader.join().unwrap_or_default();
        if too_large {
            return Err(PackliftError::Message(
                "The downloaded data exceeds the size limit.".into(),
            ));
        }
        if let Some(error) = read_error {
            return Err(error.into());
        }
        let stderr = err_reader.join().unwrap_or_default();
        if !status.success() {
            let detail: String = String::from_utf8_lossy(&stderr)
                .chars()
                .take(2500)
                .collect();
            self.write(&detail);
            return Err(PackliftError::Message(format!(
                "{} failed: {detail}",
                command[0]
            )));
        }
        Ok(data)
    }

    pub fn available(&self, executable: &str) -> bool {
        if let Ok(available) = self.available.lock() {
            if let Some(result) = available.get(executable) {
                return *result;
            }
        }
        let result = self
            .run(
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
            .is_ok_and(|output| output.status.success());
        if let Ok(mut available) = self.available.lock() {
            available.insert(executable.to_owned(), result);
        }
        result
    }

    pub fn invalidate_available(&self, executable: &str) {
        if let Ok(mut available) = self.available.lock() {
            available.remove(executable);
        }
    }

    pub fn write(&self, text: &str) {
        if let Ok(mut log) = self.log_file.lock() {
            let bytes = text.len() as u64 + 1;
            if log.bytes.saturating_add(bytes) > LOG_LIMIT {
                let _ = log.writer.flush();
                if rotate_log(&self.log_path).is_ok() {
                    if let Ok(file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.log_path)
                    {
                        log.writer = BufWriter::new(file);
                        log.bytes = 0;
                    }
                }
            }
            if log.writer.write_all(text.as_bytes()).is_ok() && log.writer.write_all(b"\n").is_ok()
            {
                log.bytes = log.bytes.saturating_add(bytes);
            }
        }
        (self.output)(text);
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

fn open_log(path: &Path) -> Result<LogFile> {
    let bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
    if bytes > LOG_LIMIT {
        rotate_log(path)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let bytes = file.metadata()?.len();
    Ok(LogFile {
        writer: BufWriter::new(file),
        bytes,
    })
}

fn rotate_log(path: &Path) -> std::io::Result<()> {
    let first = path.with_extension("log.1");
    let second = path.with_extension("log.2");
    if second.exists() {
        fs::remove_file(&second)?;
    }
    if first.exists() {
        fs::rename(&first, &second)?;
    }
    if path.exists() {
        fs::rename(path, first)?;
    }
    Ok(())
}

pub fn host_access_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        !Path::new("/.flatpak-info").is_file()
            || (!cfg!(feature = "sandbox-only")
                && Command::new("flatpak-spawn")
                    .args(["--host", "/usr/bin/true"])
                    .output()
                    .is_ok_and(|output| output.status.success()))
    })
}

pub fn packlift_data_dir() -> PathBuf {
    if !host_access_available() {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
            return PathBuf::from(data).join("flatapk");
        }
    }
    data_home().join("flatapk")
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
    connection: Arc<Mutex<Connection>>,
}

impl Registry {
    pub fn new(data: &Path) -> Result<Self> {
        fs::create_dir_all(data)?;
        let connection = Connection::open(data.join("apps.sqlite"))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            return Err(PackliftError::Message(
                "The Packlift database was created by a newer version.".into(),
            ));
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS apps (id TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS containers (name TEXT PRIMARY KEY);
             PRAGMA user_version=1;",
        )?;
        let registry = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        Ok(registry)
    }

    fn connect(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| PackliftError::Message("The application database is unavailable.".into()))
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
        .truncate(false)
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
    sha256_with_progress(path, |_, _| {})
}

pub fn sha256_with_progress(path: &Path, mut progress: impl FnMut(u64, u64)) -> Result<String> {
    let mut file = File::open(path)?;
    let total = file.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    let mut completed = 0_u64;
    loop {
        if cancellation_requested() {
            return Err(PackliftError::Message("Operation canceled.".into()));
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        completed = completed.saturating_add(count as u64);
        progress(completed, total);
    }
    progress(total, total);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn remove_exports(app: &Value, applications: &Path, data: &Path) {
    let id = app.get("id").and_then(Value::as_str).unwrap_or_default();
    if let Some(exports) = app.get("exports").and_then(Value::as_array) {
        for entry in exports {
            if let Some(path) = entry.get("window_desktop").and_then(Value::as_str) {
                let path = Path::new(path);
                if managed_regular_file(path, applications)
                    && fs::read_to_string(path).is_ok_and(|entry| {
                        entry
                            .lines()
                            .any(|line| line == format!("X-Packlift-Window-Owner={}", id))
                    })
                {
                    let _ = fs::remove_file(path);
                }
            }
            if let Some(path) = entry.get("desktop").and_then(Value::as_str) {
                let path = Path::new(path);
                let owned_name = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(&format!(".{id}.")));
                if owned_name
                    && managed_regular_file(path, applications)
                    && fs::read_to_string(path).is_ok_and(|entry| {
                        entry.lines().any(|line| line == "X-Flatapk-Managed=true")
                    })
                {
                    let _ = fs::remove_file(path);
                }
            }
            if let Some(path) = entry.get("menu_icon_file").and_then(Value::as_str) {
                let path = Path::new(path);
                if managed_regular_file(path, &data.join("package-icons")) {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}

fn managed_regular_file(path: &Path, root: &Path) -> bool {
    let (Ok(root), Some(parent), Ok(metadata)) = (
        root.canonicalize(),
        path.parent(),
        fs::symlink_metadata(path),
    ) else {
        return false;
    };
    metadata.file_type().is_file()
        && parent
            .canonicalize()
            .is_ok_and(|parent| parent.starts_with(root))
}

pub fn export_desktop(
    path: &Path,
    name: &str,
    argv: &[String],
    icon: &str,
    terminal: bool,
    category: &str,
) -> Result<()> {
    export_desktop_with_wm_class(path, name, argv, icon, terminal, category, None)
}

pub fn export_desktop_with_wm_class(
    path: &Path,
    name: &str,
    argv: &[String],
    icon: &str,
    terminal: bool,
    category: &str,
    wm_class: Option<&str>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| PackliftError::Message("Invalid desktop shortcut path.".into()))?;
    fs::create_dir_all(parent)?;
    let exec = desktop_exec(argv);
    let wm_class = wm_class
        .map(|class| format!("StartupWMClass={}\n", desktop_string(class)))
        .unwrap_or_default();
    let data = format!("[Desktop Entry]\nType=Application\nName={}\nExec={}\nIcon={}\nTerminal={}\nCategories={};\n{}X-Flatapk-Managed=true\n",
        desktop_string(name), exec, desktop_string(icon), if terminal { "true" } else { "false" }, desktop_string(category), wm_class);
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, data)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn desktop_string(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

pub(crate) fn desktop_exec(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| desktop_exec_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
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
