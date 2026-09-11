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
    /// Register blindspot as a login item. On by default: a launcher that is not running
    /// after a reboot is not a launcher.
    pub launch_at_login: bool,
    pub frecency: Frecency,
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
            launch_at_login: true,
            frecency: Frecency::default(),
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
