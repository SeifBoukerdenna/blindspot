//! What is settable, where a value came from, and the overrides the settings window writes.
//!
//! Three hardcoded things and no machinery: a static [`SCHEMA`], a flat map of
//! [`Overrides`], and a pair of `match`es that read and write a [`Config`] field by key.
//! A reflection layer over twenty knobs would be more code than the twenty knobs, and
//! CLAUDE.md's non-goals are explicit that features here get hardcoded.
//!
//! **`config.toml` is never written.** It is the user's file, read first and treated as the
//! floor; anything the window sets lands in [`Overrides`] and wins. That is the rule
//! `agent-model` already follows — see `crate::agent::session::model_file`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::config::Config;

/// Which page of the settings window a knob belongs on.
///
/// Crosses the FFI as its name rather than a tag: the shell draws the string, so a
/// numbering the two sides have to agree on would be a constant table for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Ranking,
    Content,
    Clipboard,
    Agent,
    /// Read-only rows: what is indexed, what is reachable, what is on disk.
    Status,
}

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Ranking => "Ranking",
            Self::Content => "Content",
            Self::Clipboard => "Clipboard",
            Self::Agent => "Agent",
            Self::Status => "Status",
        }
    }
}

/// What a knob holds, which is what decides both how the window draws it and how a typed
/// string is validated on the way back in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Flag,
    Count {
        min: u64,
        max: u64,
    },
    Number {
        min: f64,
        max: f64,
    },
    Text,
    /// A hotkey, validated through [`crate::hotkey::parse`] so the window's recorder can
    /// never disagree with the config file's parser.
    Chord,
    /// A list of directories.
    Paths,
    /// A diagnostic. Never written.
    Readonly,
}

impl Kind {
    /// The tag the shell switches on to pick a control.
    pub fn tag(self) -> u8 {
        match self {
            Self::Flag => 0,
            Self::Count { .. } => 1,
            Self::Number { .. } => 2,
            Self::Text => 3,
            Self::Chord => 4,
            Self::Paths => 5,
            Self::Readonly => 6,
        }
    }

    /// The bounds a stepper should respect, as a pair the FFI can carry whatever the kind.
    pub fn bounds(self) -> (f64, f64) {
        match self {
            #[expect(
                clippy::cast_precision_loss,
                reason = "bounds are small literals in the schema, not arbitrary u64s"
            )]
            Self::Count { min, max } => (min as f64, max as f64),
            Self::Number { min, max } => (min, max),
            _ => (0.0, 0.0),
        }
    }
}

/// One knob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingDef {
    /// The dotted TOML path, which is also the override key and the FFI's identity for
    /// this row. `agent.model`, not `model`.
    pub key: &'static str,
    pub section: Section,
    pub label: &'static str,
    /// One line, drawn under the control.
    pub help: &'static str,
    pub kind: Kind,
    /// Whether a change takes effect without a restart. The row says which.
    pub live: bool,
}

/// Everything settable. The order is the order the window draws.
pub static SCHEMA: &[SettingDef] = &[
    SettingDef {
        key: "hotkey",
        section: Section::General,
        label: "Hotkey",
        help: "Opens the launcher. Spotlight holds ⌘Space until you unbind it by hand.",
        kind: Kind::Chord,
        live: true,
    },
    SettingDef {
        key: "agent_hotkey",
        section: Section::General,
        label: "Agent hotkey",
        help: "Opens straight into the agent, as if > had been typed. Empty for none.",
        kind: Kind::Chord,
        live: true,
    },
    SettingDef {
        key: "launch_at_login",
        section: Section::General,
        label: "Launch at login",
        help: "A launcher that is not running after a reboot is not a launcher.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "fallback_search",
        section: Section::General,
        label: "Fallback web search",
        help: "Offered when a search finds little. An http or https address with {query} where the text goes; leave empty to hide it.",
        kind: Kind::Text,
        live: true,
    },
    SettingDef {
        key: "max_results",
        section: Section::General,
        label: "Results shown",
        help: "Rows before the list scrolls.",
        kind: Kind::Count { min: 1, max: 50 },
        live: true,
    },
    SettingDef {
        key: "app_paths",
        section: Section::General,
        label: "App folders",
        help: "Where bundles are looked for. ~ is expanded.",
        kind: Kind::Paths,
        live: true,
    },
    SettingDef {
        key: "frecency.half_life_days",
        section: Section::Ranking,
        label: "Frecency half-life",
        help: "Days for a launch to count half as much. Zero or less never decays.",
        kind: Kind::Number {
            min: 0.0,
            max: 365.0,
        },
        live: true,
    },
    SettingDef {
        key: "content.enabled",
        section: Section::Content,
        label: "Local content search",
        help: "Index text in Desktop and Downloads by default. Off stops indexing and hides results; stored excerpts are retained.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "content.semantic",
        section: Section::Content,
        label: "Search by meaning",
        help: "Add on-device semantic matches for indexed English text. Uses an installed local model; ordinary text search remains available. Off retains vectors until Erase index.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "content.roots",
        section: Section::Content,
        label: "Indexed folders",
        help: "Choose up to 32 local folders. Use Add folder to open the macOS folder picker.",
        kind: Kind::Paths,
        live: true,
    },
    SettingDef {
        key: "content.excluded_paths",
        section: Section::Content,
        label: "Excluded paths",
        help: "These paths, hidden folders, common credentials and generated dependencies are excluded.",
        kind: Kind::Paths,
        live: true,
    },
    SettingDef {
        key: "content.max_file_mb",
        section: Section::Content,
        label: "Maximum source size (MiB)",
        help: "Larger files are skipped. At most 64 KiB of text is indexed from each supported file.",
        kind: Kind::Count { min: 1, max: 16 },
        live: true,
    },
    SettingDef {
        key: "content.on_battery",
        section: Section::Content,
        label: "Index on battery",
        help: "Allow background indexing on battery. Low Power Mode and serious thermal pressure still pause work.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "content.documents",
        section: Section::Content,
        label: "Index PDF and Word documents",
        help: "Extract text from PDF, DOCX, DOC, RTF and ODT files in the indexed folders using an isolated local helper with no network access. Scanned, locked and oversized documents are counted but have no searchable body.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "content.max_document_mb",
        section: Section::Content,
        label: "Maximum document size (MiB)",
        help: "PDF files larger than this are listed by filename only. At most 64 KiB of text and 100 pages are indexed per document.",
        kind: Kind::Count { min: 1, max: 128 },
        live: true,
    },
    SettingDef {
        key: "content.low_impact",
        section: Section::Content,
        label: "Low-impact indexing",
        help: "Pause longer between batches so indexing uses less sustained CPU and disk. Passes take longer. This is pacing, not an operating-system CPU or memory quota.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "clips.enabled",
        section: Section::Clipboard,
        label: "Clipboard history",
        help: "Off stops the poller: nothing is recorded and nothing is kept.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "clips.keep",
        section: Section::Clipboard,
        label: "Clips kept",
        help: "The oldest go first. Lowering this drops them straight away.",
        kind: Kind::Count { min: 1, max: 2000 },
        live: true,
    },
    SettingDef {
        key: "clips.images",
        section: Section::Clipboard,
        label: "Record images",
        help: "Off means text only. Marked and concealed copies are always refused.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "clips.ocr",
        section: Section::Clipboard,
        label: "Read text in images",
        help: "On-device, the same recogniser as Live Text, so a screenshot is searchable.",
        kind: Kind::Flag,
        live: true,
    },
    SettingDef {
        key: "agent.enabled",
        section: Section::Agent,
        label: "Agent",
        help: "Off means blindspot opens no socket at all.",
        kind: Kind::Flag,
        live: false,
    },
    SettingDef {
        key: "agent.model",
        section: Section::Agent,
        label: "Model",
        help: "Asked for chores — a request that names a folder.",
        kind: Kind::Text,
        live: true,
    },
    SettingDef {
        key: "agent.question_model",
        section: Section::Agent,
        label: "Question model",
        help: "Asked for everything else. Empty means use the model above for both.",
        kind: Kind::Text,
        live: true,
    },
    SettingDef {
        key: "agent.host",
        section: Section::Agent,
        label: "Host",
        help: "Loopback only, checked on every request.",
        kind: Kind::Text,
        live: true,
    },
    SettingDef {
        key: "agent.keep_alive",
        section: Section::Agent,
        label: "Keep alive",
        help: "How long Ollama holds the model in memory. A cold load costs seconds.",
        kind: Kind::Text,
        live: true,
    },
    SettingDef {
        key: "agent.timeout_secs",
        section: Section::Agent,
        label: "Command timeout",
        help: "Seconds one command may run. ↩ on a running command leaves it running.",
        kind: Kind::Count { min: 1, max: 3600 },
        live: true,
    },
    SettingDef {
        key: "agent.roots",
        section: Section::Agent,
        label: "Roots",
        help: "The only folders commands may touch. Anything outside is refused.",
        kind: Kind::Paths,
        live: true,
    },
];

pub fn def(key: &str) -> Option<&'static SettingDef> {
    SCHEMA.iter().find(|d| d.key == key)
}

/// Where the value in force came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    /// Nothing set it; this is the built-in.
    Default = 0,
    /// The user's hand-edited config.toml.
    ConfigFile = 1,
    /// The settings window.
    Override = 2,
}

/// One row as the window shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub def: &'static SettingDef,
    /// The value in force.
    pub value: String,
    /// What it would be with the override removed — which is what the reset button
    /// restores, and not necessarily the built-in default.
    pub fallback: String,
    pub source: Source,
}

// ---------------------------------------------------------------------------
// Reading and writing a Config field by key
// ---------------------------------------------------------------------------

/// A path list crosses the FFI NUL-separated: a macOS path may contain any byte except
/// `/` and NUL, so NUL is the only separator that cannot appear inside a member. The
/// boundary already passes pointer + length rather than C strings for the same reason.
pub const LIST_SEPARATOR: char = '\0';

fn join(items: &[String]) -> String {
    items.join(&LIST_SEPARATOR.to_string())
}

fn split(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value
        .split(LIST_SEPARATOR)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// The value of `key` in `config`, in the string form the FFI carries.
///
/// Paired with [`write`]: every key in [`SCHEMA`] must appear in both, which
/// `schema_and_config_agree` asserts.
pub fn read(config: &Config, key: &str) -> Option<String> {
    Some(match key {
        "hotkey" => config.hotkey.clone(),
        "fallback_search" => config.fallback_search.clone(),
        "agent_hotkey" => config.agent_hotkey.clone(),
        "launch_at_login" => config.launch_at_login.to_string(),
        "max_results" => config.max_results.to_string(),
        "app_paths" => join(&config.app_paths),
        "frecency.half_life_days" => config.frecency.half_life_days.to_string(),
        "agent.enabled" => config.agent.enabled.to_string(),
        "agent.model" => config.agent.model.clone(),
        "agent.question_model" => config.agent.question_model.clone(),
        "agent.host" => config.agent.host.clone(),
        "agent.keep_alive" => config.agent.keep_alive.clone(),
        "agent.timeout_secs" => config.agent.timeout_secs.to_string(),
        "agent.roots" => join(&config.agent.roots),
        "clips.enabled" => config.clips.enabled.to_string(),
        "clips.keep" => config.clips.keep.to_string(),
        "clips.images" => config.clips.images.to_string(),
        "clips.ocr" => config.clips.ocr.to_string(),
        "content.enabled" => config.content.enabled.to_string(),
        "content.semantic" => config.content.semantic.to_string(),
        "content.roots" => join(&config.content.roots),
        "content.excluded_paths" => join(&config.content.excluded_paths),
        "content.max_file_mb" => config.content.max_file_mb.to_string(),
        "content.on_battery" => config.content.on_battery.to_string(),
        "content.documents" => config.content.documents.to_string(),
        "content.max_document_mb" => config.content.max_document_mb.to_string(),
        "content.low_impact" => config.content.low_impact.to_string(),
        _ => return None,
    })
}

/// Puts an already-validated `value` into `config`. Returns false for a key that is not
/// in the schema.
fn write(config: &mut Config, key: &str, value: &str) -> bool {
    match key {
        "hotkey" => config.hotkey = value.to_owned(),
        "fallback_search" => config.fallback_search = value.trim().to_owned(),
        "agent_hotkey" => config.agent_hotkey = value.to_owned(),
        "launch_at_login" => config.launch_at_login = value == "true",
        "max_results" => match value.parse() {
            Ok(n) => config.max_results = n,
            Err(_) => return false,
        },
        "app_paths" => config.app_paths = split(value),
        "frecency.half_life_days" => match value.parse() {
            Ok(n) => config.frecency.half_life_days = n,
            Err(_) => return false,
        },
        "agent.enabled" => config.agent.enabled = value == "true",
        "agent.model" => config.agent.model = value.to_owned(),
        "agent.question_model" => config.agent.question_model = value.to_owned(),
        "agent.host" => config.agent.host = value.to_owned(),
        "agent.keep_alive" => config.agent.keep_alive = value.to_owned(),
        "agent.timeout_secs" => match value.parse() {
            Ok(n) => config.agent.timeout_secs = n,
            Err(_) => return false,
        },
        "agent.roots" => config.agent.roots = split(value),
        "clips.enabled" => config.clips.enabled = value == "true",
        "clips.keep" => match value.parse() {
            Ok(n) => config.clips.keep = n,
            Err(_) => return false,
        },
        "clips.images" => config.clips.images = value == "true",
        "clips.ocr" => config.clips.ocr = value == "true",
        "content.enabled" => config.content.enabled = value == "true",
        "content.semantic" => config.content.semantic = value == "true",
        "content.roots" => config.content.roots = split(value),
        "content.excluded_paths" => config.content.excluded_paths = split(value),
        "content.max_file_mb" => match value.parse() {
            Ok(n) => config.content.max_file_mb = n,
            Err(_) => return false,
        },
        "content.on_battery" => config.content.on_battery = value == "true",
        "content.documents" => config.content.documents = value == "true",
        "content.max_document_mb" => match value.parse() {
            Ok(megabytes) => config.content.max_document_mb = megabytes,
            Err(_) => return false,
        },
        "content.low_impact" => config.content.low_impact = value == "true",
        _ => return false,
    }
    true
}

/// Checks `value` against the knob's kind, returning the text to show the user on a
/// refusal.
///
/// Every parser lives here rather than in Swift, so the window's hotkey recorder cannot
/// drift from the parser config.toml is read with.
pub fn validate(def: &SettingDef, value: &str) -> Result<(), String> {
    match def.kind {
        Kind::Flag => match value {
            "true" | "false" => Ok(()),
            other => Err(format!("expected true or false, not {other:?}")),
        },
        Kind::Count { min, max } => match value.parse::<u64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(()),
            Ok(n) => Err(format!("{n} is outside {min}–{max}")),
            Err(_) => Err(format!("{value:?} is not a whole number")),
        },
        Kind::Number { min, max } => match value.parse::<f64>() {
            Ok(n) if n.is_finite() && (min..=max).contains(&n) => Ok(()),
            Ok(_) => Err(format!("expected a number between {min} and {max}")),
            Err(_) => Err(format!("{value:?} is not a number")),
        },
        // An empty agent hotkey is "no second hotkey", which is a legitimate answer and
        // the default. Every other chord goes through the real parser.
        Kind::Chord if value.trim().is_empty() && def.key == "agent_hotkey" => Ok(()),
        Kind::Chord => crate::hotkey::parse(value.trim())
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Kind::Text if def.key == "agent.host" => {
            let probe = crate::config::Agent {
                host: value.to_owned(),
                ..crate::config::Agent::default()
            };
            if probe.is_loopback() {
                Ok(())
            } else {
                Err(format!("must be loopback, not {value:?}"))
            }
        }
        Kind::Text if def.key == "fallback_search" => {
            let value = value.trim();
            if value.is_empty() || (crate::shortcuts::valid_url(value) && value.contains("{query}")) {
                Ok(())
            } else {
                Err("must be an http or https address containing {query}, or empty".to_owned())
            }
        }
        Kind::Text => Ok(()),
        Kind::Paths if def.key.starts_with("content.") => {
            let paths = split(value);
            let limit = if def.key == "content.roots" { 32 } else { 128 };
            if paths.len() > limit
                || paths.iter().any(|path| {
                    path.len() > 4096
                        || !(path.starts_with('/') || path == "~" || path.starts_with("~/"))
                        || std::path::Path::new(path)
                            .components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                })
            {
                Err(format!(
                    "Choose at most {limit} absolute or ~/ paths without parent traversal"
                ))
            } else {
                Ok(())
            }
        }
        Kind::Paths => {
            if split(value)
                .iter()
                .all(|p| p.starts_with('/') || p.starts_with('~'))
            {
                Ok(())
            } else {
                Err("every folder must be absolute or start with ~".to_owned())
            }
        }
        Kind::Readonly => Err("this row is not settable".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// The overrides file
// ---------------------------------------------------------------------------

/// What the settings window has set, keyed by the same dotted paths as [`SCHEMA`].
///
/// Flat rather than nested so applying it is one lookup per key and the file can be read
/// at a glance. Values are kept in the same string form the FFI carries, because that is
/// the only form the schema knows how to validate — with the one exception of a path
/// list, which is stored as a TOML array since TOML cannot hold the NUL that separates
/// them on the wire.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Overrides {
    values: BTreeMap<String, String>,
    /// A field rather than a constant so tests never write to the real state directory —
    /// the same reason `Session::model_path` is one.
    path: Option<PathBuf>,
    damaged: bool,
}

impl Overrides {
    pub fn default_path() -> Option<PathBuf> {
        crate::store::data_dir()
            .ok()
            .map(|d| d.join("overrides.toml"))
    }

    /// Invalid files leave defaults usable and block writes until the file is repaired.
    pub fn load(path: Option<PathBuf>) -> Self {
        let loaded = (|| -> std::io::Result<BTreeMap<String, String>> {
            let Some(path) = &path else {
                return Ok(BTreeMap::new());
            };
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(BTreeMap::new());
                }
                Err(error) => return Err(error),
            };
            let mut text = String::new();
            file.take(262_145).read_to_string(&mut text)?;
            if text.len() > 262_144 {
                return Err(std::io::Error::other("Settings file exceeds limit"));
            }
            let table = toml::from_str::<toml::Table>(&text)
                .map_err(|_| std::io::Error::other("Invalid settings file"))?;
            Ok(Self::decode(&table))
        })();
        match loaded {
            Ok(values) => Self {
                values,
                path,
                damaged: false,
            },
            Err(_) => {
                eprintln!(
                    "blindspot: settings overrides unavailable; preserving the file and using defaults"
                );
                Self {
                    values: BTreeMap::new(),
                    path,
                    damaged: true,
                }
            }
        }
    }

    fn decode(table: &toml::Table) -> BTreeMap<String, String> {
        table
            .iter()
            .filter_map(|(key, value)| {
                let def = def(key)?;
                let text = match (def.kind, value) {
                    (Kind::Paths, toml::Value::Array(items)) => join(
                        &items
                            .iter()
                            .map(|v| v.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()?,
                    ),
                    (_, toml::Value::String(s)) => s.clone(),
                    _ => return None,
                };
                validate(def, &text).ok()?;
                Some((key.clone(), text))
            })
            .collect()
    }

    fn encode(&self) -> toml::Table {
        self.values
            .iter()
            .filter_map(|(key, value)| {
                let def = def(key)?;
                let encoded = if def.kind == Kind::Paths {
                    toml::Value::Array(split(value).into_iter().map(toml::Value::String).collect())
                } else {
                    toml::Value::String(value.clone())
                };
                Some((key.clone(), encoded))
            })
            .collect()
    }

    fn save(&self) -> Result<(), &'static str> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if self.damaged {
            return Err(
                "Existing settings could not be read; repair overrides.toml before saving changes",
            );
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text =
            toml::to_string_pretty(&self.encode()).map_err(|_| "Settings could not be encoded")?;
        if text.len() > 262_144 {
            return Err("Settings exceed the storage limit");
        }
        let parent = path.parent().ok_or("Settings directory unavailable")?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|_| "Settings directory unavailable")?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Settings clock unavailable")?
            .as_nanos();
        for attempt in 0..8 {
            let temporary = parent.join(format!(
                ".blindspot-overrides-{}-{nonce}-{attempt}.tmp",
                std::process::id()
            ));
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    return Err("Settings could not be saved; the previous values are unchanged");
                }
            };
            let result = file
                .write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .and_then(|()| std::fs::rename(&temporary, path));
            drop(file);
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
                return Err("Settings could not be saved; the previous values are unchanged");
            }
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
            return Ok(());
        }
        Err("Settings temporary file unavailable; try again")
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Records `value` for `key` and writes the file. The caller has already validated.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), &'static str> {
        let mut next = self.clone();
        next.values.insert(key.to_owned(), value.to_owned());
        next.save()?;
        *self = next;
        Ok(())
    }

    /// Forgets `key`, so its value falls back to config.toml and then to the built-in.
    pub fn reset(&mut self, key: &str) -> Result<(), &'static str> {
        let mut next = self.clone();
        next.values.remove(key);
        next.save()?;
        *self = next;
        Ok(())
    }

    /// Lays the overrides over a config parsed from config.toml.
    pub fn apply(&self, config: &mut Config) {
        for (key, value) in &self.values {
            if !write(config, key, value) {
                eprintln!("blindspot: override {key:?} is not a setting; ignoring it");
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
}

/// Every row the settings window draws, in schema order.
///
/// `file` is the config as parsed from config.toml with no overrides applied — which is
/// what makes "where did this value come from" answerable, and what the reset button
/// restores to.
pub fn resolve(file: &Config, overrides: &Overrides) -> Vec<Resolved> {
    let built_in = Config::default();
    SCHEMA
        .iter()
        .filter_map(|def| {
            let fallback = read(file, def.key)?;
            let (value, source) = match overrides.get(def.key) {
                Some(set) => (set.to_owned(), Source::Override),
                None if fallback == read(&built_in, def.key)? => {
                    (fallback.clone(), Source::Default)
                }
                None => (fallback.clone(), Source::ConfigFile),
            };
            Some(Resolved {
                def,
                value,
                fallback,
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blindspot-settings-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("overrides.toml")
    }

    #[test]
    fn schema_and_config_agree() {
        // The schema and the two `match`es are three hardcoded lists. A key in one and not
        // the others is a silent no-op, which is exactly the bug this closes.
        let mut config = Config::default();
        for def in SCHEMA {
            let value = read(&config, def.key)
                .unwrap_or_else(|| panic!("{} is in the schema but `read` skips it", def.key));
            assert!(
                write(&mut config, def.key, &value),
                "{} is in the schema but `write` skips it",
                def.key
            );
            assert_eq!(
                read(&config, def.key).as_deref(),
                Some(value.as_str()),
                "{} did not survive a read/write round trip",
                def.key
            );
        }
    }

    #[test]
    fn every_default_passes_its_own_validation() {
        let config = Config::default();
        for def in SCHEMA {
            let value = read(&config, def.key).expect("in schema");
            assert_eq!(
                validate(def, &value),
                Ok(()),
                "the default for {} does not validate",
                def.key
            );
        }
    }

    #[test]
    fn a_value_is_default_config_or_override() {
        let file = Config {
            max_results: 12,
            ..Config::default()
        };
        let mut overrides = Overrides::default();
        overrides.values.insert("agent.model".into(), "x".into());

        let rows = resolve(&file, &overrides);
        let by = |key: &str| rows.iter().find(|r| r.def.key == key).expect("row");

        assert_eq!(by("hotkey").source, Source::Default);
        assert_eq!(by("max_results").source, Source::ConfigFile);
        assert_eq!(by("max_results").value, "12");
        assert_eq!(by("agent.model").source, Source::Override);
        assert_eq!(by("agent.model").value, "x");
        // The reset button restores config.toml's answer, not the built-in one.
        assert_eq!(by("agent.model").fallback, Config::default().agent.model);
    }

    #[test]
    fn an_override_wins_over_the_file() {
        let mut config = Config {
            max_results: 3,
            ..Config::default()
        };
        let mut overrides = Overrides::default();
        overrides.values.insert("max_results".into(), "9".into());
        overrides.apply(&mut config);
        assert_eq!(config.max_results, 9);
    }

    #[test]
    fn overrides_round_trip_through_the_file() {
        let path = scratch("round-trip");
        let mut overrides = Overrides::load(Some(path.clone()));
        overrides.set("max_results", "11").expect("saved");
        overrides.set("agent.roots", "~/dev\0/tmp").expect("saved");
        overrides
            .set("agent.model", "qwen3.5:9b-mlx")
            .expect("saved");

        let reloaded = Overrides::load(Some(path));
        assert_eq!(reloaded.get("max_results"), Some("11"));
        assert_eq!(reloaded.get("agent.model"), Some("qwen3.5:9b-mlx"));
        // A path list survives as an array, because TOML cannot hold the NUL that
        // separates one on the wire.
        assert_eq!(reloaded.get("agent.roots"), Some("~/dev\0/tmp"));
    }

    #[test]
    fn resetting_forgets_the_key() {
        let path = scratch("reset");
        let mut overrides = Overrides::load(Some(path.clone()));
        overrides.set("max_results", "11").expect("saved");
        overrides.reset("max_results").expect("saved");
        assert!(Overrides::load(Some(path)).is_empty());
    }

    #[test]
    fn a_corrupt_overrides_file_is_ignored_not_fatal() {
        let path = scratch("corrupt");
        std::fs::write(&path, "this is not toml {{{").expect("write");
        assert!(Overrides::load(Some(path)).is_empty());
    }

    #[test]
    fn an_unknown_key_in_the_file_is_dropped() {
        let path = scratch("unknown");
        std::fs::write(
            &path,
            "\"not.a.setting\" = \"1\"\n\"max_results\" = \"4\"\n",
        )
        .expect("write");
        let overrides = Overrides::load(Some(path));
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides.get("max_results"), Some("4"));
    }

    #[test]
    fn failed_save_preserves_effective_values_and_previous_file() {
        let path = scratch("failed-save");
        let mut overrides = Overrides::load(Some(path.clone()));
        overrides.set("content.enabled", "true").expect("saved");
        let previous = std::fs::read(&path).expect("previous");
        let backup = path.with_extension("backup");
        std::fs::rename(&path, &backup).expect("preserve fixture");
        std::fs::create_dir(&path).expect("block replacement");
        assert!(overrides.set("content.enabled", "false").is_err());
        assert!(overrides.reset("content.enabled").is_err());
        assert_eq!(overrides.get("content.enabled"), Some("true"));
        assert_eq!(std::fs::read(&backup).expect("retained"), previous);
        std::fs::remove_dir(&path).expect("unblock");
        std::fs::rename(backup, &path).expect("restore fixture");
        overrides.set("content.enabled", "false").expect("retry");
        assert_eq!(
            Overrides::load(Some(path.clone())).get("content.enabled"),
            Some("false")
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(path.parent().expect("parent"))
                .expect("entries")
                .count(),
            1
        );
    }

    #[test]
    fn malformed_or_oversized_settings_are_preserved_and_invalid_values_are_ignored() {
        let path = scratch("preserved-invalid");
        for bytes in ["private malformed {{{".to_owned(), "x".repeat(262_145)] {
            std::fs::write(&path, &bytes).expect("fixture");
            let mut overrides = Overrides::load(Some(path.clone()));
            assert!(overrides.set("content.enabled", "false").is_err());
            assert_eq!(std::fs::read_to_string(&path).expect("preserved"), bytes);
        }
        std::fs::write(&path, "\"max_results\" = \"99999999\"\n\"agent.host\" = \"external.invalid:11434\"\n\"content.enabled\" = \"true\"\n").expect("values");
        let overrides = Overrides::load(Some(path));
        assert_eq!(overrides.len(), 1);
        let mut config = Config::default();
        overrides.apply(&mut config);
        assert!(config.content.enabled);
        assert_eq!(config.max_results, Config::default().max_results);
        assert_eq!(config.agent.host, Config::default().agent.host);
    }

    #[test]
    fn semantic_setting_round_trips_and_mixed_path_arrays_are_rejected_as_a_whole() {
        let path = scratch("semantic-paths");
        let mut overrides = Overrides::load(Some(path.clone()));
        overrides.set("content.semantic", "true").unwrap();
        let mut config = Config::default();
        Overrides::load(Some(path.clone())).apply(&mut config);
        assert!(config.content.semantic);
        assert!(config.content.enabled);
        std::fs::write(&path, "\"content.excluded_paths\" = [\"/private\", 123]\n").unwrap();
        let invalid = Overrides::load(Some(path));
        assert!(invalid.get("content.excluded_paths").is_none());
    }

    #[test]
    fn validation_refuses_what_the_kind_forbids() {
        let count = def("max_results").expect("in schema");
        assert!(validate(count, "0").is_err(), "below the minimum");
        assert!(validate(count, "51").is_err(), "above the maximum");
        assert!(validate(count, "eight").is_err());
        assert_eq!(validate(count, "8"), Ok(()));

        let chord = def("hotkey").expect("in schema");
        assert!(
            validate(chord, "shift+space").is_err(),
            "needs a real modifier"
        );
        assert_eq!(validate(chord, "cmd+shift+space"), Ok(()));
        // An empty hotkey is not a hotkey; an empty *agent* hotkey is "none configured".
        assert!(validate(chord, "").is_err());
        assert_eq!(
            validate(def("agent_hotkey").expect("in schema"), ""),
            Ok(())
        );

        let host = def("agent.host").expect("in schema");
        assert!(validate(host, "example.com:11434").is_err(), "not loopback");
        assert_eq!(validate(host, "127.0.0.1:11434"), Ok(()));

        let paths = def("app_paths").expect("in schema");
        assert!(validate(paths, "Applications").is_err(), "relative");
        assert_eq!(validate(paths, "/Applications\0~/Applications"), Ok(()));
    }

    #[test]
    fn a_path_list_survives_being_emptied() {
        assert!(split("").is_empty());
        assert_eq!(join(&[]), "");
    }
}
