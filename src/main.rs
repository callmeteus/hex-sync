use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GIST_FILENAME: &str = "hex-settings.json";
const SETTINGS_FILENAME: &str = "settings.json";
const STATE_FILENAME: &str = "hs-settings.json";
const LEGACY_STATE_FILENAME: &str = "gist-id";
const GIST_DESCRIPTION: &str = "Hex settings (hex-sync)";
const GIST_FORMAT: &str = "hex-sync/v1";
const LEGACY_GIST_FORMAT: &str = "hex-gist-sync/v1";
const HISTORY_LIMIT: usize = 50;
const REPOSITORY: &str = "callmeteus/hex-sync";
const SENTINEL: &str = "__HEX_SYNC_LOCAL__";

// Values that belong to one machine only. Their whole subtree is replaced by a sentinel.
const LOCAL_PATHS: &[&[&str]] = &[
    // Audio input and transcription model are tied to the machine hardware.
    &["microphone"],
    &["avoid_bluetooth_microphone"],
    &["transcription", "model"],
    &["transcription_device"],
    &["transcription_recents"],
    &["keyboard_model_switching"],
    &["experimental", "hex_microphone"],
    &["experimental", "hex_microphone_mode"],
    &["experimental", "hex_microphone_comfort_noise"],
    &["experimental", "hex_microphone_strength"],
    // Shortcuts and keyboard behavior stay on the machine where they were configured.
    &["shortcut_mode"],
    &["double_tap_lock"],
    &["double_tap_only"],
    &["dictation_hotkey"],
    &["paste_last_hotkey"],
    &["cancel_hotkey"],
    &["raw_dictation_hotkey"],
    &["typed_text_hotkey"],
    &["second_dictation_hotkey"],
    &["extra_dictation_hotkeys"],
    &["terminal_paste_hotkey"],
    &["capture", "hotkey"],
    &["capture", "replay_hotkey"],
    &["capture", "transcriber"],
    &["screenshots", "hotkey"],
    &["screenshots", "record_hotkey"],
    &["gnome_keyboard_shortcuts"],
    &["share_dictation_shortcut"],
    // Paths, integration endpoints and app bookkeeping.
    &["meetings", "output_folder"],
    &["capture", "folder"],
    &["screenshots", "folder"],
    &["capture", "routing", "jev"],
    &["capture", "routing", "jevApiKey"],
    &["capture", "routing", "agentHost"],
    &["capture", "routing", "agentOpencode"],
    &["migrations"],
];

#[derive(Debug)]
enum CommandKind {
    Init { public: bool },
    Use(String),
    Push,
    Pull,
    Diff,
    Watch { interval: u64 },
    History { revisions: bool },
    SelfUpdate { force: bool },
    InstallService,
    UninstallService,
    Help,
}

struct Options {
    command: CommandKind,
    settings_path: PathBuf,
    state_file: PathBuf,
    gist_filename: String,
    gist_id: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let options = parse_args()?;

    match &options.command {
        CommandKind::Help => {
            print_help();
            Ok(())
        }
        CommandKind::Init { public } => cmd_init(&options, *public),
        CommandKind::Use(value) => cmd_use(&options, value),
        CommandKind::Push => cmd_push(&options),
        CommandKind::Pull => cmd_pull(&options),
        CommandKind::Diff => cmd_diff(&options),
        CommandKind::Watch { interval } => cmd_watch(&options, *interval),
        CommandKind::History { revisions } => cmd_history(&options, *revisions),
        CommandKind::SelfUpdate { force } => cmd_self_update(*force),
        CommandKind::InstallService => install_service(),
        CommandKind::UninstallService => uninstall_service(),
    }
}

fn print_help() {
    println!(
        "Usage: hex-sync <command> [options]\n\
         \n\
         Commands:\n\
           init [--public]  Create a gist with the current settings and save its ID locally.\n\
           use <id|url>     Point this machine to an existing gist.\n\
           push             Upload local settings to the gist.\n\
           pull             Download settings from the gist, preserving local secrets.\n\
           diff             Show whether the local file and the gist diverged.\n\
           watch             Run a daemon that syncs changes automatically.\n\
           history           Show which machines changed the gist.\n\
           self-update       Replace the binary with the latest GitHub release.\n\
           install-service   Install the daemon as a systemd user service or scheduled task.\n\
           uninstall-service Remove the automatic sync service.\n\
           help              Show this help.\n\
         \n\
         Watch options:\n\
           --interval <seconds>  Poll interval, default 15 (or HEX_SYNC_INTERVAL).\n\
         \n\
         History options:\n\
           --revisions           Show native GitHub gist revisions instead.\n\
         \n\
         Self-update options:\n\
           --force               Replace the binary even when the version matches.\n\
         \n\
         Environment variables:\n\
           HEX_GIST_ID          Gist ID, takes precedence over the state file.\n\
           HEX_SETTINGS_PATH    Path to the Hex settings file.\n\
           HEX_GIST_FILE        Name of the file inside the gist.\n\
           HEX_GIST_STATE_FILE  Path to the CLI config (hs-settings.json).\n\
           HEX_GIST_PUBLIC      Set to 1 with init to create a public gist.\n\
           HEX_SYNC_INTERVAL    Default watch interval in seconds."
    );
}

fn parse_args() -> Result<Options> {
    let args: Vec<String> = env::args().skip(1).collect();

    let command = match args.first().map(String::as_str) {
        Some("init") => {
            let public = args.iter().skip(1).any(|argument| argument == "--public")
                || env::var("HEX_GIST_PUBLIC")
                    .map(|value| value == "1")
                    .unwrap_or(false);

            if let Some(unknown) = args
                .iter()
                .skip(1)
                .find(|argument| argument.as_str() != "--public")
            {
                bail!("Unknown init option: {unknown}");
            }

            CommandKind::Init { public }
        }
        Some("use") => {
            let value = args.get(1).context(
                "Provide the gist ID or URL. Example: hex-sync use https://gist.github.com/user/<id>",
            )?;
            CommandKind::Use(value.clone())
        }
        Some("push") => CommandKind::Push,
        Some("pull") => CommandKind::Pull,
        Some("diff") => CommandKind::Diff,
        Some("watch") => {
            let mut interval = env::var("HEX_SYNC_INTERVAL")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(15);

            let mut index = 1;
            while index < args.len() {
                if args[index] == "--interval" || args[index] == "-i" {
                    let value = args
                        .get(index + 1)
                        .context("Missing value for --interval")?;
                    interval = value
                        .parse()
                        .context("Interval must be a number of seconds")?;
                    index += 2;
                } else if let Some(value) = args[index].strip_prefix("--interval=") {
                    interval = value
                        .parse()
                        .context("Interval must be a number of seconds")?;
                    index += 1;
                } else {
                    bail!("Unknown watch option: {}", args[index]);
                }
            }

            if interval == 0 {
                bail!("Interval must be at least 1 second");
            }

            CommandKind::Watch { interval }
        }
        Some("history") => {
            let revisions = args
                .iter()
                .skip(1)
                .any(|argument| argument == "--revisions");

            if let Some(unknown) = args
                .iter()
                .skip(1)
                .find(|argument| argument.as_str() != "--revisions")
            {
                bail!("Unknown history option: {unknown}");
            }

            CommandKind::History { revisions }
        }
        Some("self-update") => {
            let force = args.iter().skip(1).any(|argument| argument == "--force");

            if let Some(unknown) = args
                .iter()
                .skip(1)
                .find(|argument| argument.as_str() != "--force")
            {
                bail!("Unknown self-update option: {unknown}");
            }

            CommandKind::SelfUpdate { force }
        }
        Some("install-service") => CommandKind::InstallService,
        Some("uninstall-service") => CommandKind::UninstallService,
        Some("help") | Some("--help") | Some("-h") | None => CommandKind::Help,
        Some(other) => bail!("Unknown command: {other}"),
    };

    let settings_path = env::var_os("HEX_SETTINGS_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(detect_settings_path);

    let state_file = env::var_os("HEX_GIST_STATE_FILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_state_file(&settings_path));

    let gist_filename = env::var("HEX_GIST_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| GIST_FILENAME.to_string());

    let gist_id = env::var("HEX_GIST_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| normalize_id(&value))
        .transpose()?;

    Ok(Options {
        command,
        settings_path,
        state_file,
        gist_filename,
        gist_id,
    })
}

fn detect_settings_path() -> PathBuf {
    let mut candidates = Vec::new();

    if let Some(directory) = dirs::data_dir() {
        candidates.push(directory.join("hex").join(SETTINGS_FILENAME));
    }

    if let Some(directory) = dirs::data_local_dir() {
        candidates.push(directory.join("hex").join(SETTINGS_FILENAME));
    }

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/hex").join(SETTINGS_FILENAME));
    }

    #[cfg(windows)]
    {
        // Hex has no published Windows data path, so check both AppData roots and both names.
        for variable in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(directory) = env::var_os(variable) {
                let base = PathBuf::from(directory);
                candidates.push(base.join("hex").join(SETTINGS_FILENAME));
                candidates.push(base.join("anomaly").join("hex").join(SETTINGS_FILENAME));
            }
        }
    }

    let mut seen = BTreeSet::new();
    for candidate in &candidates {
        if seen.insert(candidate.clone()) && candidate.is_file() {
            return candidate.clone();
        }
    }

    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from(SETTINGS_FILENAME))
}

fn default_state_file(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(STATE_FILENAME)
}

fn legacy_state_files(settings_path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(LEGACY_STATE_FILENAME)];

    if let Some(directory) = dirs::config_dir() {
        paths.push(directory.join("hex-sync").join(LEGACY_STATE_FILENAME));
        paths.push(directory.join("hex-gist-sync").join(LEGACY_STATE_FILENAME));
    }

    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/hex-sync").join(LEGACY_STATE_FILENAME));
        paths.push(
            home.join(".config/hex-gist-sync")
                .join(LEGACY_STATE_FILENAME),
        );
    }

    paths
}

fn read_state_file(path: &Path) -> Result<Option<String>> {
    let contents = fs::read_to_string(path)?;

    if let Ok(value) = serde_json::from_str::<Value>(&contents) {
        if let Some(id) = value.get("gistId").and_then(Value::as_str) {
            return Ok(Some(id.to_string()));
        }
    }

    // Earlier versions stored the bare ID as plain text.
    let trimmed = contents.trim();
    if !trimmed.is_empty() {
        return Ok(Some(trimmed.to_string()));
    }

    Ok(None)
}

fn normalize_id(value: &str) -> Result<String> {
    let id = if value.starts_with("http") {
        value
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(value)
    } else {
        value
    };

    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid gist ID: {id}");
    }

    Ok(id.to_ascii_lowercase())
}

fn is_local_path(path: &[String]) -> bool {
    LOCAL_PATHS.iter().any(|candidate| {
        candidate.len() == path.len()
            && candidate
                .iter()
                .zip(path)
                .all(|(expected, actual)| *expected == actual.as_str())
    })
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "apikey",
        "api_key",
        "api-key",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "auth",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

fn sanitize(value: &Value, path: &[String]) -> Value {
    let is_local =
        is_local_path(path) || path.last().map(|key| is_secret_key(key)).unwrap_or(false);

    if is_local {
        return Value::String(SENTINEL.to_string());
    }

    match value {
        Value::Object(map) => {
            let mut result = Map::new();

            for (key, item) in map {
                let mut child = path.to_vec();
                child.push(key.clone());
                result.insert(key.clone(), sanitize(item, &child));
            }

            Value::Object(result)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| sanitize(item, path)).collect())
        }
        _ => value.clone(),
    }
}

fn get_path<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = root;

    for key in path {
        current = current.get(key)?;
    }

    Some(current)
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    if path.is_empty() {
        return;
    }

    let mut current = root;

    for key in &path[..path.len() - 1] {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }

        current = current
            .as_object_mut()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }

    if !current.is_object() {
        *current = Value::Object(Map::new());
    }

    let last = path.last().unwrap();
    current.as_object_mut().unwrap().insert(last.clone(), value);
}

fn merge(remote: &Value, local: Option<&Value>, path: &[String]) -> Option<Value> {
    if is_local_path(path) {
        // Omit the key when this machine has no value, so the app falls back to its default.
        return local.cloned();
    }

    match remote {
        Value::Object(map) => {
            let mut result = Map::new();

            for (key, item) in map {
                let mut child = path.to_vec();
                child.push(key.clone());
                let local_child = local.and_then(|value| value.get(key));

                if let Some(value) = merge(item, local_child, &child) {
                    result.insert(key.clone(), value);
                }
            }

            Some(Value::Object(result))
        }
        Value::Array(items) => {
            let local_items: Vec<&Value> = local
                .and_then(Value::as_array)
                .map(|array| array.iter().collect())
                .unwrap_or_default();

            let merged = items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| merge(item, local_items.get(index).copied(), path))
                .collect();

            Some(Value::Array(merged))
        }
        Value::String(value) if value == SENTINEL => Some(match local {
            Some(value) if !value.is_object() && !value.is_array() => value.clone(),
            _ => Value::Null,
        }),
        _ => Some(remote.clone()),
    }
}

fn copy_local_secrets(value: &Value, path: &[String], merged: &mut Value) {
    let is_local =
        is_local_path(path) || path.last().map(|key| is_secret_key(key)).unwrap_or(false);

    if is_local {
        // Keep the match instead of is_none_or so the crate still builds on Rust 1.70.
        let missing = match get_path(merged, path) {
            Some(value) => value.is_null(),
            None => true,
        };
        if !value.is_null() && missing {
            set_path(merged, path, value.clone());
        }
        return;
    }

    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let mut child = path.to_vec();
                child.push(key.clone());
                copy_local_secrets(item, &child, merged);
            }
        }
        Value::Array(items) => {
            for item in items {
                copy_local_secrets(item, path, merged);
            }
        }
        _ => {}
    }
}

fn collect_differences(left: &Value, right: &Value, path: &[String], result: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();

            for key in keys {
                let mut child = path.to_vec();
                child.push(key.clone());

                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => collect_differences(x, y, &child, result),
                    (Some(_), None) => result.push(format!("only local: {}", child.join("."))),
                    (None, Some(_)) => result.push(format!("only gist: {}", child.join("."))),
                    _ => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                result.push(format!("length differs: {}", path.join(".")));
            }

            for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                let mut child = path.to_vec();
                child.push(index.to_string());
                collect_differences(x, y, &child, result);
            }
        }
        _ if left != right => result.push(path.join(".")),
        _ => {}
    }
}

fn read_settings(path: &Path) -> Result<Value> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Hex settings file not found at: {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&contents).context("settings file is not valid JSON")?;

    if !value.is_object() {
        bail!("settings file must contain a JSON object");
    }

    Ok(value)
}

fn to_json_string(value: &Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn require_auth() -> Result<()> {
    run_gh(&["auth", "status"], None).map(|_| ())
}

fn run_gh(arguments: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut command = Command::new("gh");
    command
        .args(arguments)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("GitHub CLI (gh) not found. Install it from https://cli.github.com/")?;

    if let Some(contents) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(contents.as_bytes())
            .context("Could not write to gh stdin")?;
    }

    let output = child.wait_with_output().context("Could not wait for gh")?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if message.is_empty() {
            bail!("gh exited with status {}", output.status);
        }
        bail!("{message}");
    }

    String::from_utf8(output.stdout).context("gh returned non-UTF-8 output")
}

fn resolve_gist_id(options: &Options) -> Result<String> {
    if let Some(id) = &options.gist_id {
        return Ok(id.clone());
    }

    if options.state_file.is_file() {
        if let Some(id) = read_state_file(&options.state_file)? {
            return normalize_id(&id);
        }
    }

    for path in legacy_state_files(&options.settings_path) {
        if path.is_file() {
            if let Some(id) = read_state_file(&path)? {
                return normalize_id(&id);
            }
        }
    }

    bail!(
        "No gist configured. Run 'init' or 'use <id>' first. (config file: {})",
        options.state_file.display()
    )
}

fn save_gist_id(path: &Path, id: &str) -> Result<()> {
    let mut config = Map::new();

    if let Ok(contents) = fs::read_to_string(path) {
        if let Ok(value) = serde_json::from_str::<Value>(&contents) {
            if let Some(object) = value.as_object() {
                config = object.clone();
            }
        }
    }

    config.insert("version".to_string(), Value::from(1));
    config.insert("gistId".to_string(), Value::String(id.to_string()));

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, to_json_string(&Value::Object(config))?)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(contents: &str) -> Result<Self> {
        let path = env::temp_dir().join(format!(
            "hex-sync-{}-{}.json",
            std::process::id(),
            timestamp_nanos()
        ));
        fs::write(&path, contents)?;
        Ok(Self { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn write_settings(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let staging = path.with_extension(format!("hex-sync-{}.tmp", timestamp_nanos()));
    fs::write(&staging, to_json_string(value)?)?;

    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&staging, metadata.permissions())?;
    }

    fs::rename(&staging, path).with_context(|| format!("Could not replace {}", path.display()))?;

    Ok(())
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    // Howard Hinnant's civil calendar conversion, valid for the Unix epoch and beyond.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };

    if month <= 2 {
        year += 1;
    }

    (year, month, day)
}

fn format_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

fn machine_name() -> String {
    env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .or_else(|_| fs::read_to_string("/etc/hostname").map(|name| name.trim().to_string()))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn user_name() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn history_entry(action: &str) -> Value {
    let mut entry = Map::new();
    entry.insert("at".to_string(), Value::String(format_timestamp()));
    entry.insert("machine".to_string(), Value::String(machine_name()));
    entry.insert("user".to_string(), Value::String(user_name()));
    entry.insert("os".to_string(), Value::String(env::consts::OS.to_string()));
    entry.insert("action".to_string(), Value::String(action.to_string()));
    Value::Object(entry)
}

fn extract_settings(document: &Value) -> Result<Value> {
    let format = document.get("format").and_then(Value::as_str);

    if format == Some(GIST_FORMAT) || format == Some(LEGACY_GIST_FORMAT) {
        if let Some(settings) = document.get("settings") {
            if settings.is_object() {
                return Ok(settings.clone());
            }
        }
    }

    // Gists created by earlier versions contain the settings object directly.
    if document.is_object() {
        return Ok(document.clone());
    }

    bail!("The gist file must contain a JSON object")
}

fn extract_history(document: &Value) -> Vec<Value> {
    document
        .get("signature")
        .and_then(|signature| signature.get("history"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn build_document(settings: &Value, history: &[Value]) -> Value {
    let mut signature = Map::new();
    signature.insert("tool".to_string(), Value::String("hex-sync".to_string()));
    signature.insert("version".to_string(), Value::from(1));
    signature.insert("history".to_string(), Value::Array(history.to_vec()));

    let mut document = Map::new();
    document.insert("format".to_string(), Value::String(GIST_FORMAT.to_string()));
    document.insert("signature".to_string(), Value::Object(signature));
    document.insert("settings".to_string(), settings.clone());
    Value::Object(document)
}

fn fetch_document(options: &Options, id: &str) -> Result<Value> {
    let raw = fetch_remote_raw(options, id)?;
    let document: Value = serde_json::from_str(&raw).context("The gist file is not valid JSON")?;

    if !document.is_object() {
        bail!("The gist file must contain a JSON object");
    }

    Ok(document)
}

fn fetch_remote_raw(options: &Options, id: &str) -> Result<String> {
    let raw = run_gh(
        &[
            "gist",
            "view",
            id,
            "--filename",
            &options.gist_filename,
            "--raw",
        ],
        None,
    )?;

    Ok(raw.trim_end().to_string())
}

fn fetch_remote(options: &Options, id: &str) -> Result<Value> {
    let document = fetch_document(options, id)?;
    extract_settings(&document)
}

fn cmd_init(options: &Options, public: bool) -> Result<()> {
    require_auth()?;

    let settings = read_settings(&options.settings_path)?;
    let sanitized = to_json_string(&build_document(
        &sanitize(&settings, &[]),
        &[history_entry("init")],
    ))?;

    let mut arguments = vec![
        "gist".to_string(),
        "create".to_string(),
        "--desc".to_string(),
        GIST_DESCRIPTION.to_string(),
        "--filename".to_string(),
        options.gist_filename.clone(),
    ];

    if public {
        arguments.push("--public".to_string());
    }

    arguments.push("-".to_string());
    let references: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let output = run_gh(&references, Some(&sanitized))?;

    let url = output
        .lines()
        .find(|line| line.starts_with("https://gist.github.com/"))
        .context("Gist created, but its URL could not be identified in the gh output")?;

    let id = normalize_id(url.trim().trim_end_matches('/'))?;
    save_gist_id(&options.state_file, &id)?;

    println!("Gist ID saved to: {}", options.state_file.display());
    println!("Gist created: {}", url.trim());

    Ok(())
}

fn cmd_use(options: &Options, value: &str) -> Result<()> {
    let id = normalize_id(value)?;
    save_gist_id(&options.state_file, &id)?;
    println!("Gist ID saved to: {}", options.state_file.display());
    Ok(())
}

fn cmd_push(options: &Options) -> Result<()> {
    require_auth()?;
    let id = resolve_gist_id(options)?;
    push_to_gist(options, &id, "push")?;
    println!("Settings uploaded to gist {id}.");
    Ok(())
}

fn push_to_gist(options: &Options, id: &str, action: &str) -> Result<()> {
    let settings = read_settings(&options.settings_path)?;
    let document = fetch_document(options, id)?;
    let mut history = extract_history(&document);
    history.push(history_entry(action));

    if history.len() > HISTORY_LIMIT {
        let excess = history.len() - HISTORY_LIMIT;
        history.drain(..excess);
    }

    let sanitized = to_json_string(&build_document(&sanitize(&settings, &[]), &history))?;
    let temporary = TempFile::new(&sanitized)?;
    let path = temporary.path.to_string_lossy().into_owned();

    run_gh(
        &[
            "gist",
            "edit",
            id,
            "--filename",
            &options.gist_filename,
            &path,
        ],
        None,
    )?;

    Ok(())
}

fn cmd_pull(options: &Options) -> Result<()> {
    require_auth()?;
    let id = resolve_gist_id(options)?;
    let backup = pull_from_gist(options, &id)?;

    if let Some(path) = backup {
        println!("Backup saved to: {}", path.display());
    }

    println!("Settings downloaded from gist {id}.");
    println!("Restart Hex if it is running, so it reloads the settings.");

    Ok(())
}

fn pull_from_gist(options: &Options, id: &str) -> Result<Option<PathBuf>> {
    let remote = fetch_remote(options, id)?;
    let local = if options.settings_path.is_file() {
        Some(read_settings(&options.settings_path)?)
    } else {
        None
    };

    let mut merged =
        merge(&remote, local.as_ref(), &[]).unwrap_or_else(|| Value::Object(Map::new()));
    let mut backup_path = None;

    if let Some(local_value) = &local {
        copy_local_secrets(local_value, &[], &mut merged);

        let backup = options
            .settings_path
            .with_extension(format!("bak.{}", timestamp_secs()));
        fs::copy(&options.settings_path, &backup)?;
        backup_path = Some(backup);
    }

    write_settings(&options.settings_path, &merged)?;

    Ok(backup_path)
}

fn cmd_diff(options: &Options) -> Result<()> {
    require_auth()?;

    let id = resolve_gist_id(options)?;
    let settings = read_settings(&options.settings_path)?;
    let local = sanitize(&settings, &[]);
    let remote = fetch_remote(options, &id)?;

    let mut differences = Vec::new();
    collect_differences(&local, &remote, &[], &mut differences);

    if differences.is_empty() {
        println!("Local settings and gist are in sync.");
    } else {
        println!("Settings diverged. Run push or pull.");
        for difference in differences {
            println!("  {difference}");
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "hex-sync.service";

#[cfg(target_os = "windows")]
const WINDOWS_TASK: &str = "HexSync";

fn local_snapshot(options: &Options) -> Result<String> {
    let settings = read_settings(&options.settings_path)?;
    Ok(to_json_string(&sanitize(&settings, &[]))?
        .trim_end()
        .to_string())
}

fn remote_snapshot(options: &Options, id: &str) -> Result<String> {
    let document = fetch_document(options, id)?;
    let settings = extract_settings(&document)?;
    Ok(to_json_string(&settings)?.trim_end().to_string())
}

fn release_asset_name() -> Result<String> {
    let asset = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "hex-sync-linux-x86_64",
        ("windows", "x86_64") => "hex-sync-windows-x86_64.exe",
        (os, arch) => bail!("No release binary published for {os} on {arch}"),
    };

    Ok(asset.to_string())
}

fn latest_release() -> Result<(String, String)> {
    require_auth()?;

    let raw = run_gh(
        &["api", &format!("repos/{REPOSITORY}/releases/latest")],
        None,
    )?;
    let release: Value =
        serde_json::from_str(&raw).context("Invalid release response from gh api")?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .context("Release has no tag_name")?
        .to_string();
    let asset = release_asset_name()?;
    let available = release
        .get("assets")
        .and_then(Value::as_array)
        .map(|assets| {
            assets
                .iter()
                .any(|item| item.get("name").and_then(Value::as_str) == Some(asset.as_str()))
        })
        .unwrap_or(false);

    if !available {
        bail!("Release {tag} has no asset named {asset}");
    }

    Ok((tag, asset))
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self> {
        let path = env::temp_dir().join(format!(
            "hex-sync-update-{}-{}",
            std::process::id(),
            timestamp_nanos()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn download_release(tag: &str, asset: &str, destination: &Path) -> Result<PathBuf> {
    let directory = destination.to_string_lossy().into_owned();

    run_gh(
        &[
            "release",
            "download",
            tag,
            "--pattern",
            asset,
            "--dir",
            &directory,
            "--clobber",
        ],
        None,
    )?;

    let file = destination.join(asset);

    if !file.is_file() {
        bail!("Release asset was not downloaded: {asset}");
    }

    Ok(file)
}

#[cfg(unix)]
fn replace_binary(downloaded: &Path, current: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(downloaded, fs::Permissions::from_mode(0o755))?;
    fs::rename(downloaded, current)
        .with_context(|| format!("Could not replace {}", current.display()))
}

#[cfg(windows)]
fn replace_binary(downloaded: &Path, current: &Path) -> Result<()> {
    // Windows locks the running executable, so a detached batch applies the swap after exit.
    let pending = current.with_extension("new.exe");
    fs::copy(downloaded, &pending)?;

    let script = env::temp_dir().join(format!("hex-sync-update-{}.cmd", timestamp_nanos()));
    let contents = format!(
        "@echo off\n:wait\nping -n 2 127.0.0.1 >nul\nmove /Y \"{}\" \"{}\" >nul\nif exist \"{}\" goto wait\ndel \"%~f0\"\n",
        pending.display(),
        current.display(),
        pending.display()
    );
    fs::write(&script, contents)?;

    let script_path = script.to_string_lossy().into_owned();
    Command::new("cmd")
        .args(["/C", "start", "", "/MIN", &script_path])
        .spawn()
        .context("Could not start the update script")?;

    Ok(())
}

fn cmd_self_update(force: bool) -> Result<()> {
    let (tag, asset) = latest_release()?;
    let current_version = env!("CARGO_PKG_VERSION");

    if !force && tag.trim_start_matches('v') == current_version {
        println!("Already up to date ({current_version}).");
        return Ok(());
    }

    let temporary = TempDir::new()?;
    let downloaded = download_release(&tag, &asset, &temporary.path)?;
    let executable = env::current_exe()?.canonicalize()?;
    replace_binary(&downloaded, &executable)?;

    println!("Updated {current_version} -> {tag}.");

    #[cfg(windows)]
    println!("The update is applied as this process exits.");

    Ok(())
}

fn cmd_history(options: &Options, revisions: bool) -> Result<()> {
    require_auth()?;

    let id = resolve_gist_id(options)?;

    if revisions {
        let raw = run_gh(
            &["api", &format!("gists/{id}/revisions?per_page=100")],
            None,
        )?;
        let response: Value = serde_json::from_str(&raw).context("Invalid response from gh api")?;
        let items = response
            .as_array()
            .context("Unexpected gist revisions response")?;

        println!("GitHub gist revisions: {}", items.len());

        for revision in items.iter().take(20) {
            let time = revision
                .get("committed_at")
                .and_then(Value::as_str)
                .unwrap_or("unknown time");
            let changes = revision.get("change_status").map(Value::to_string);
            match changes {
                Some(changes) => println!("  {time}  {changes}"),
                None => println!("  {time}"),
            }
        }

        return Ok(());
    }

    let document = fetch_document(options, &id)?;
    let history = extract_history(&document);

    if history.is_empty() {
        println!("No signature history in this gist yet.");
        return Ok(());
    }

    for entry in history {
        let time = entry.get("at").and_then(Value::as_str).unwrap_or("?");
        let machine = entry.get("machine").and_then(Value::as_str).unwrap_or("?");
        let user = entry.get("user").and_then(Value::as_str).unwrap_or("?");
        let os = entry.get("os").and_then(Value::as_str).unwrap_or("?");
        let action = entry.get("action").and_then(Value::as_str).unwrap_or("?");

        println!("{time} | {action} | {machine} | {user} | {os}");
    }

    Ok(())
}

fn cmd_watch(options: &Options, interval: u64) -> Result<()> {
    require_auth()?;

    let id = resolve_gist_id(options)?;

    if !options.settings_path.is_file() {
        println!("[watch] No local settings found, pulling from gist {id}.");
        pull_from_gist(options, &id)?;
    }

    let mut local_baseline = local_snapshot(options)?;
    let mut remote_baseline = remote_snapshot(options, &id)?;
    let mut conflict_reported = false;

    println!("[watch] Watching every {interval}s. Press Ctrl+C to stop.");

    let mut last_update_check: Option<Instant> = None;

    loop {
        let due_for_check = match last_update_check {
            Some(instant) => instant.elapsed() >= Duration::from_secs(6 * 60 * 60),
            None => true,
        };

        if due_for_check {
            last_update_check = Some(Instant::now());

            if let Ok((tag, _)) = latest_release() {
                if tag.trim_start_matches('v') != env!("CARGO_PKG_VERSION") {
                    println!("[watch] Update available: {tag}. Run 'hex-sync self-update'.");
                }
            }
        }

        sleep(Duration::from_secs(interval));

        let local = match local_snapshot(options) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[watch] Could not read local settings: {error:#}");
                continue;
            }
        };

        let remote = match remote_snapshot(options, &id) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[watch] Could not read gist: {error:#}");
                continue;
            }
        };

        let local_changed = local != local_baseline;
        let remote_changed = remote != remote_baseline;

        if !local_changed && !remote_changed {
            conflict_reported = false;
            continue;
        }

        if local_changed && remote_changed {
            if !conflict_reported {
                eprintln!(
                    "[watch] Conflict: settings and gist both changed. Run push or pull manually."
                );
                conflict_reported = true;
            }
            continue;
        }

        if local_changed {
            match push_to_gist(options, &id, "watch") {
                Ok(()) => {
                    println!("[watch] Pushed local changes to gist {id}.");
                    local_baseline = local;
                    remote_baseline = match remote_snapshot(options, &id) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("[watch] Could not re-read gist: {error:#}");
                            remote_baseline.clone()
                        }
                    };
                    conflict_reported = false;
                }
                Err(error) => eprintln!("[watch] Push failed: {error:#}"),
            }
        } else {
            match pull_from_gist(options, &id) {
                Ok(_) => {
                    println!("[watch] Pulled gist changes into local settings.");
                    remote_baseline = remote;
                    local_baseline = match local_snapshot(options) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("[watch] Could not re-read local settings: {error:#}");
                            continue;
                        }
                    };
                    conflict_reported = false;
                }
                Err(error) => eprintln!("[watch] Pull failed: {error:#}"),
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn environment_lines() -> Vec<String> {
    [
        "HEX_GIST_ID",
        "HEX_SETTINGS_PATH",
        "HEX_GIST_FILE",
        "HEX_GIST_STATE_FILE",
        "HEX_GIST_PUBLIC",
        "HEX_SYNC_INTERVAL",
    ]
    .iter()
    .filter_map(|name| {
        env::var(name)
            .ok()
            .map(|value| format!("Environment={name}={value}"))
    })
    .collect()
}

#[cfg(target_os = "linux")]
fn service_file_path() -> Result<PathBuf> {
    let directory = dirs::config_dir()
        .context("Could not find the user config directory")?
        .join("systemd/user");

    Ok(directory.join(SYSTEMD_SERVICE))
}

#[cfg(target_os = "linux")]
fn install_service() -> Result<()> {
    let executable = env::current_exe()?.canonicalize()?;
    let path = service_file_path()?;
    fs::create_dir_all(path.parent().context("Service path has no parent")?)?;

    let mut content = format!(
        "[Unit]\nDescription=Hex Sync daemon\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart=\"{}\" watch\nRestart=always\nRestartSec=5\n",
        executable.display()
    );

    for line in environment_lines() {
        content.push_str(&line);
        content.push('\n');
    }

    content.push_str("\n[Install]\nWantedBy=default.target\n");
    fs::write(&path, content)?;

    println!("Service file written to: {}", path.display());

    let reloaded = Command::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
        .status();

    if !matches!(reloaded, Ok(status) if status.success()) {
        println!("Start the service with: systemctl --user daemon-reload");
        return Ok(());
    }

    let enabled = Command::new("systemctl")
        .arg("--user")
        .args(["enable", "--now", SYSTEMD_SERVICE])
        .status()
        .context("Could not run systemctl")?;

    if enabled.success() {
        println!(
            "Service enabled and started. Check with: systemctl --user status {SYSTEMD_SERVICE}"
        );
    } else {
        println!("Enable it with: systemctl --user enable --now {SYSTEMD_SERVICE}");
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_service() -> Result<()> {
    let path = service_file_path()?;

    if !path.is_file() {
        bail!("Service file not found: {}", path.display());
    }

    let _ = Command::new("systemctl")
        .arg("--user")
        .args(["disable", "--now", SYSTEMD_SERVICE])
        .status();

    fs::remove_file(&path)?;
    println!("Service file removed: {}", path.display());

    Ok(())
}

#[cfg(target_os = "windows")]
fn install_service() -> Result<()> {
    let executable = env::current_exe()?.canonicalize()?;
    let command = format!("\"{}\" watch", executable.display());

    let created = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            WINDOWS_TASK,
            "/TR",
            &command,
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/F",
        ])
        .status()
        .context("Could not run schtasks")?;

    if !created.success() {
        bail!("schtasks failed to create the task");
    }

    let _ = Command::new("schtasks")
        .args(["/Run", "/TN", WINDOWS_TASK])
        .status();

    println!("Task '{WINDOWS_TASK}' created and started. It runs on every logon.");

    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_service() -> Result<()> {
    let deleted = Command::new("schtasks")
        .args(["/Delete", "/TN", WINDOWS_TASK, "/F"])
        .status()
        .context("Could not run schtasks")?;

    if !deleted.success() {
        bail!("schtasks failed to delete the task");
    }

    println!("Task '{WINDOWS_TASK}' removed.");

    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn install_service() -> Result<()> {
    bail!("Automatic service installation is supported only on Linux and Windows")
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn uninstall_service() -> Result<()> {
    bail!("Automatic service installation is supported only on Linux and Windows")
}
