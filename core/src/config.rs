//! Config loading from `~/.config/blindspot/config.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where app bundles are looked for when the config file does not say.
///
/// Pending confirmation from research C: whether first-party apps are reachable
/// at these paths on macOS 26 or only via their cryptex mount points.
const DEFAULT_APP_PATHS: &[&str] = &[
    "/Applications",
    "/Applications/Utilities",
    "/System/Applications",
    "/System/Applications/Utilities",
    "~/Applications",
    "/System/Library/CoreServices",
];

const DEFAULT_MAX_RESULTS: usize = 8;

/// Unknown keys are ignored rather than rejected. The config is hand-edited and a
/// stray key must not take down the daemon — losing the hotkey entirely is a far
/// worse outcome than silently skipping a typo.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub max_results: usize,
    pub app_paths: Vec<String>,
    /// The global hotkey, e.g. `"cmd+shift+space"`. See [`crate::hotkey::parse`].
    pub hotkey: String,
    /// A second hotkey that opens straight into the agent, as if `>` had been typed. Empty
    /// means only the one hotkey.
    pub agent_hotkey: String,
    /// Register blindspot as a login item. On by default: a launcher that is not running
    /// after a reboot is not a launcher.
    pub launch_at_login: bool,
    /// Web search offered as a fallback when a launcher search finds little; `{query}` is filled
    /// with the typed text. Empty hides the row. Only opened when chosen.
    pub fallback_search: String,
    pub frecency: Frecency,
    pub agent: Agent,
    pub clips: Clips,
    pub content: Content,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Content {
    pub enabled: bool,
    pub semantic: bool,
    pub embedding_model: String,
    #[serde(skip)]
    pub embedding_host: String,
    pub code_roots: Vec<String>,
    pub index_budget_mb: u64,
    pub ocr: bool,
    pub ocr_pages: u32,
    pub roots: Vec<String>,
    pub excluded_paths: Vec<String>,
    pub max_file_mb: u64,
    pub on_battery: bool,
    /// Extract text from PDF documents in the chosen folders through the isolated helper.
    pub documents: bool,
    pub max_document_mb: u64,
    /// Longer pauses between batches. Slower passes in exchange for less sustained CPU and I/O;
    /// macOS offers no per-app CPU quota to enforce instead.
    pub low_impact: bool,
}

impl Default for Content {
    fn default() -> Self {
        let roots = crate::files::home_dir()
            .map(|home| {
                vec![
                    home.join("Desktop").to_string_lossy().into_owned(),
                    home.join("Downloads").to_string_lossy().into_owned(),
                ]
            })
            .unwrap_or_default();
        Self {
            enabled: true,
            semantic: false,
            embedding_model: "embeddinggemma:300m".into(),
            embedding_host: "127.0.0.1:11434".into(),
            code_roots: Vec::new(),
            index_budget_mb: 3072,
            ocr: false,
            ocr_pages: 20,
            roots,
            excluded_paths: vec!["~/Library".into(), "~/.ssh".into(), "~/.gnupg".into()],
            max_file_mb: 1,
            on_battery: false,
            documents: true,
            max_document_mb: 32,
            low_impact: false,
        }
    }
}

impl Content {
    pub fn expanded_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .chain(self.code_roots.iter())
            .take(32)
            .filter_map(|path| expand_tilde(path))
            .collect()
    }

    pub fn expanded_code_roots(&self) -> Vec<PathBuf> {
        self.code_roots.iter().take(32).filter_map(|path| expand_tilde(path)).collect()
    }

    pub fn expanded_exclusions(&self) -> Vec<PathBuf> {
        self.excluded_paths
            .iter()
            .take(128)
            .filter_map(|path| expand_tilde(path))
            .collect()
    }
}

/// Clipboard history (M5).
///
/// Only what is a preference. The per-item and total byte ceilings in `clips.rs` stay
/// constants: their comments describe a disk-safety invariant — eviction runs until both
/// limits hold — and a 10 GB cap is a bug rather than a choice.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Clips {
    /// Off stops the poller entirely, so nothing is recorded and nothing is kept.
    pub enabled: bool,
    /// How many clips to keep. The oldest go first.
    pub keep: usize,
    /// Whether to record images at all. Off means text only.
    pub images: bool,
    /// Whether to read the text in an image, on-device, so a screenshot is searchable by
    /// what it says. Costs a Vision pass per image copied.
    pub ocr: bool,
}

impl Default for Clips {
    fn default() -> Self {
        Self {
            enabled: true,
            keep: crate::clips::DEFAULT_KEEP,
            images: true,
            ocr: true,
        }
    }
}

/// The local agent (M6). Off with `enabled = false`, which stops blindspot from opening a
/// socket at all.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Agent {
    pub enabled: bool,
    /// An Ollama model name. The default was chosen by measurement — see CLAUDE.md; the
    /// smaller `qwen3.5:0.8b-mlx` is about three times faster and measurably sloppier.
    pub model: String,
    /// The model for questions, which want reasoning more than speed. Empty means "use
    /// `model` for everything". Which one is asked is decided by whether the request names a
    /// folder; the model itself still decides whether to answer or propose commands.
    pub question_model: String,
    /// Host and port, loopback only. A non-loopback host is refused at load: this feature is
    /// local by design, not by configuration.
    pub host: String,
    /// How long Ollama keeps the model in memory. Long, because a cold load costs seconds.
    pub keep_alive: String,
    /// The cap on one command's run time.
    pub timeout_secs: u64,
    /// Directories commands may touch. Anything outside is refused.
    pub roots: Vec<String>,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "qwen3.5:4b-mlx".to_owned(),
            question_model: "qwen3.8:27b-mlx".to_owned(),
            host: "127.0.0.1:11434".to_owned(),
            keep_alive: "30m".to_owned(),
            timeout_secs: 120,
            roots: vec!["~".to_owned()],
        }
    }
}

impl Agent {
    /// The directories commands may touch, `~` expanded.
    pub fn expanded_roots(&self) -> Vec<PathBuf> {
        self.roots.iter().filter_map(|r| expand_tilde(r)).collect()
    }

    /// Whether the configured host is loopback. Checked rather than trusted: "no network" is
    /// the promise, and a typo in config.toml must not quietly send your requests elsewhere.
    pub fn is_loopback(&self) -> bool {
        let host = self.host.rsplit_once(':').map_or(&*self.host, |(h, _)| h);
        matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
    }
}

/// Read at M1 so the config schema is stable, but not consulted until M3.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Frecency {
    pub half_life_days: f64,
}

impl Default for Frecency {
    fn default() -> Self {
        Self {
            half_life_days: 14.0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_MAX_RESULTS,
            app_paths: DEFAULT_APP_PATHS.iter().map(|s| (*s).to_owned()).collect(),
            // ⌘⇧Space, not ⌘Space: see `hotkey::DEFAULT` for why the obvious default would
            // register cleanly and then never fire.
            hotkey: "cmd+shift+space".to_owned(),
            agent_hotkey: String::new(),
            launch_at_login: true,
            fallback_search: "https://duckduckgo.com/?q={query}".to_owned(),
            frecency: Frecency::default(),
            agent: Agent::default(),
            clips: Clips::default(),
            content: Content::default(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "could not read config: {e}"),
            Self::Parse(e) => write!(f, "could not parse config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// The default config location, or `None` if `$HOME` is unset.
    pub fn default_path() -> Option<PathBuf> {
        home_dir().map(|h| h.join(".config/blindspot/config.toml"))
    }

    /// Loads `path`. A missing file yields defaults; a malformed one is an error the
    /// caller is expected to fall back from rather than propagate.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(ConfigError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Read(e)),
        }
    }

    /// The agent hotkey, if one is configured and parses. An unparseable one is dropped with a
    /// line on stderr rather than falling back: a second hotkey nobody asked for is worse than
    /// none, and the first hotkey still opens the agent with `>`.
    pub fn agent_hotkey(&self) -> Option<crate::hotkey::Hotkey> {
        let spec = self.agent_hotkey.trim();
        if spec.is_empty() {
            return None;
        }
        match crate::hotkey::parse(spec) {
            Ok(hotkey) => Some(hotkey),
            Err(e) => {
                eprintln!("blindspot: agent_hotkey {spec:?}: {e}; ignoring it");
                None
            }
        }
    }

    /// The configured hotkey, or the default plus `false` if the config's value does not
    /// parse. A typo must cost the user their custom binding, never their launcher.
    pub fn hotkey(&self) -> (crate::hotkey::Hotkey, bool) {
        match crate::hotkey::parse(&self.hotkey) {
            Ok(hotkey) => (hotkey, true),
            Err(e) => {
                eprintln!(
                    "blindspot: hotkey {:?}: {e}; using cmd+shift+space",
                    self.hotkey
                );
                (crate::hotkey::DEFAULT, false)
            }
        }
    }

    /// `app_paths` with `~` expanded. Entries needing `$HOME` are dropped if it is unset.
    pub fn resolved_app_paths(&self) -> Vec<PathBuf> {
        self.app_paths
            .iter()
            .filter_map(|p| expand_tilde(p))
            .collect()
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn expand_tilde(path: &str) -> Option<PathBuf> {
    expand_tilde_with(path, home_dir().as_deref())
}

/// Split out from [`expand_tilde`] so it can be tested without mutating `$HOME`,
/// which is process-global and would race across parallel tests.
fn expand_tilde_with(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix('~') {
        // `~` alone or `~/...`, but not `~user`, which we do not support.
        Some("") => home.map(PathBuf::from),
        Some(rest) => match rest.strip_prefix('/') {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(path)),
        },
        None => Some(PathBuf::from(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_not_an_error() {
        let cfg = Config::load(Path::new("/nonexistent/blindspot/config.toml"))
            .expect("missing file should fall back to defaults");
        assert_eq!(cfg.max_results, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn partial_config_keeps_defaults_for_absent_keys() {
        let cfg: Config = toml::from_str("max_results = 3").expect("parses");
        assert_eq!(cfg.max_results, 3);
        assert_eq!(cfg.frecency.half_life_days, 14.0);
        assert!(!cfg.app_paths.is_empty());
    }

    #[test]
    fn unknown_keys_do_not_fail_the_load() {
        let cfg: Config =
            toml::from_str("max_results = 5\nsomething_from_v2 = true").expect("parses");
        assert_eq!(cfg.max_results, 5);
    }

    #[test]
    fn a_bad_hotkey_falls_back_rather_than_failing() {
        let cfg: Config = toml::from_str("hotkey = \"cmd+nope\"").expect("parses");
        assert_eq!(cfg.hotkey(), (crate::hotkey::DEFAULT, false));
        let cfg: Config = toml::from_str("hotkey = \"ctrl+opt+k\"").expect("parses");
        assert!(cfg.hotkey().1);
    }

    #[test]
    fn launch_at_login_defaults_on_and_can_be_turned_off() {
        assert!(Config::default().launch_at_login);
        let cfg: Config = toml::from_str("launch_at_login = false").expect("parses");
        assert!(!cfg.launch_at_login);
    }

    #[test]
    fn frecency_table_is_parsed() {
        let cfg: Config = toml::from_str("[frecency]\nhalf_life_days = 30.0").expect("parses");
        assert_eq!(cfg.frecency.half_life_days, 30.0);
    }

    #[test]
    fn tilde_expands_only_at_a_path_boundary() {
        let home = Path::new("/Users/test");
        let e = |p| expand_tilde_with(p, Some(home));
        assert_eq!(e("~/Applications"), Some("/Users/test/Applications".into()));
        assert_eq!(e("~"), Some("/Users/test".into()));
        assert_eq!(e("/Applications"), Some("/Applications".into()));
        // `~foo` is not a home-relative path we understand; it stays literal.
        assert_eq!(e("~foo"), Some("~foo".into()));
    }

    #[test]
    fn home_relative_paths_are_dropped_when_home_is_unset() {
        assert_eq!(expand_tilde_with("~/Applications", None), None);
        // Absolute paths still resolve with no home.
        assert_eq!(
            expand_tilde_with("/Applications", None),
            Some("/Applications".into())
        );
    }
}
