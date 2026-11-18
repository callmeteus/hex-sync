use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const GIST_FILENAME: &str = "hex-settings.json";
const GIST_DESCRIPTION: &str = "HEx settings (hex-gist-sync)";
const SENTINEL: &str = "__HEX_SYNC_LOCAL__";

// Values that belong to one machine only. Their whole subtree is replaced by a sentinel.
const LOCAL_PATHS: &[&[&str]] = &[
    &["microphone"],
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
    }
}

fn print_help() {
    println!(
        "Usage: hex-gist-sync <command> [options]\n\
         \n\
         Commands:\n\
           init [--public]  Create a gist with the current settings and save its ID locally.\n\
           use <id|url>     Point this machine to an existing gist.\n\
           push             Upload local settings to the gist.\n\
           pull             Download settings from the gist, preserving local secrets.\n\
           diff             Show whether the local file and the gist diverged.\n\
           help             Show this help.\n\
         \n\
         Environment variables:\n\
           HEX_GIST_ID          Gist ID, takes precedence over the state file.\n\
           HEX_SETTINGS_PATH    Path to the HEx settings.json.\n\
           HEX_GIST_FILE        Name of the file inside the gist.\n\
           HEX_GIST_STATE_FILE  Where the gist ID is stored.\n\
           HEX_GIST_PUBLIC      Set to 1 with init to create a public gist."
    );
}

fn parse_args() -> Result<Options> {
    let args: Vec<String> = env::args().skip(1).collect();

    let command = match args.first().map(String::as_str) {
        Some("init") => {
            let public = args
                .iter()
                .skip(1)
                .any(|argument| argument == "--public")
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
                "Provide the gist ID or URL. Example: hex-gist-sync use https://gist.github.com/user/<id>",
            )?;
            CommandKind::Use(value.clone())
        }
        Some("push") => CommandKind::Push,
        Some("pull") => CommandKind::Pull,
        Some("diff") => CommandKind::Diff,
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
        .unwrap_or_else(default_state_file);

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
        candidates.push(directory.join("hex").join("settings.json"));
    }

    if let Some(directory) = dirs::data_local_dir() {
        candidates.push(directory.join("hex").join("settings.json"));
    }

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/hex/settings.json"));
    }

    #[cfg(windows)]
    {
        // HEx has no published Windows data path, so check both AppData roots and both names.
        for variable in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(directory) = env::var_os(variable) {
                let base = PathBuf::from(directory);
                candidates.push(base.join("hex").join("settings.json"));
                candidates.push(base.join("anomaly").join("hex").join("settings.json"));
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
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

fn default_state_file() -> PathBuf {
    if let Some(directory) = dirs::config_dir() {
        return directory.join("hex-gist-sync").join("gist-id");
    }

    if let Some(home) = dirs::home_dir() {
        return home.join(".config/hex-gist-sync/gist-id");
    }

    PathBuf::from("hex-gist-sync/gist-id")
}

fn normalize_id(value: &str) -> Result<String> {
    let id = if value.starts_with("http") {
        value.rsplit('/').find(|part| !part.is_empty()).unwrap_or(value)
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
        "apikey", "api_key", "api-key", "token", "secret", "password", "passwd", "credential",
        "auth",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

fn sanitize(value: &Value, path: &[String]) -> Value {
    let is_local = is_local_path(path)
        || path
            .last()
            .map(|key| is_secret_key(key))
            .unwrap_or(false);

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
    current
        .as_object_mut()
        .unwrap()
        .insert(last.clone(), value);
}

fn merge(remote: &Value, local: Option<&Value>, path: &[String]) -> Value {
    if is_local_path(path) {
        return local.cloned().unwrap_or(Value::Null);
    }

    match remote {
        Value::Object(map) => {
            let mut result = Map::new();

            for (key, item) in map {
                let mut child = path.to_vec();
                child.push(key.clone());
                let local_child = local.and_then(|value| value.get(key));
                result.insert(key.clone(), merge(item, local_child, &child));
            }

            Value::Object(result)
        }
        Value::Array(items) => {
            let local_items: Vec<&Value> = local
                .and_then(Value::as_array)
                .map(|array| array.iter().collect())
                .unwrap_or_default();

            Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        merge(item, local_items.get(index).copied(), path)
                    })
                    .collect(),
            )
        }
        Value::String(value) if value == SENTINEL => match local {
            Some(value) if !value.is_object() && !value.is_array() => value.clone(),
            _ => Value::Null,
        },
        _ => remote.clone(),
    }
}

fn copy_local_secrets(value: &Value, path: &[String], merged: &mut Value) {
    let is_local = is_local_path(path)
        || path
            .last()
            .map(|key| is_secret_key(key))
            .unwrap_or(false);

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
        .with_context(|| format!("HEx settings.json not found at: {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents).context("settings.json is not valid JSON")?;

    if !value.is_object() {
        bail!("settings.json must contain a JSON object");
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

    let contents = fs::read_to_string(&options.state_file).with_context(|| {
        format!(
            "No gist configured. Run 'init' or 'use <id>' first. (state file: {})",
            options.state_file.display()
        )
    })?;

    normalize_id(contents.trim())
}

fn save_gist_id(path: &Path, id: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, format!("{id}\n"))?;

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
            "hex-gist-sync-{}-{}.json",
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

    let staging = path.with_extension(format!("hex-gist-sync-{}.tmp", timestamp_nanos()));
    fs::write(&staging, to_json_string(value)?)?;

    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&staging, metadata.permissions())?;
    }

    fs::rename(&staging, path).with_context(|| format!("Could not replace {}", path.display()))?;

    Ok(())
}

fn fetch_remote(options: &Options, id: &str) -> Result<Value> {
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

    let value: Value = serde_json::from_str(&raw).context("The gist file is not valid JSON")?;

    if !value.is_object() {
        bail!("The gist file must contain a JSON object");
    }

    Ok(value)
}

fn cmd_init(options: &Options, public: bool) -> Result<()> {
    require_auth()?;

    let settings = read_settings(&options.settings_path)?;
    let sanitized = to_json_string(&sanitize(&settings, &[]))?;

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
    let settings = read_settings(&options.settings_path)?;
    let sanitized = to_json_string(&sanitize(&settings, &[]))?;
    let temporary = TempFile::new(&sanitized)?;
    let path = temporary.path.to_string_lossy().into_owned();

    run_gh(
        &[
            "gist",
            "edit",
            &id,
            "--filename",
            &options.gist_filename,
            &path,
        ],
        None,
    )?;

    println!("Settings uploaded to gist {id}.");
    Ok(())
}

fn cmd_pull(options: &Options) -> Result<()> {
    require_auth()?;

    let id = resolve_gist_id(options)?;
    let remote = fetch_remote(options, &id)?;
    let local = if options.settings_path.is_file() {
        Some(read_settings(&options.settings_path)?)
    } else {
        None
    };

    let mut merged = merge(&remote, local.as_ref(), &[]);

    if let Some(local_value) = &local {
        copy_local_secrets(local_value, &[], &mut merged);

        let backup = options
            .settings_path
            .with_extension(format!("bak.{}", timestamp_secs()));
        fs::copy(&options.settings_path, &backup)?;
        println!("Backup saved to: {}", backup.display());
    }

    write_settings(&options.settings_path, &merged)?;

    println!("Settings downloaded from gist {id}.");
    println!("Restart HEx if it is running, so it reloads the settings.");

    Ok(())
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
