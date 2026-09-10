//! Discovering application bundles and turning them into [`AppEntry`] values.

use std::path::{Path, PathBuf};

use plist::{Dictionary, Value};

use super::AppEntry;

/// How deep to look below each configured root.
///
/// One level, so `/Applications/Cisco/Cisco Secure Client.app` is found — there are
/// three such folder-nested bundles on this machine. Going deeper starts walking vendor
/// junk drawers without turning up anything a person would launch.
const MAX_DEPTH: usize = 1;

/// Whether a directory's `LSUIElement` bundles are worth indexing.
///
/// This exists because the obvious rule is wrong. `LSUIElement` means "no Dock icon",
/// not "not launchable": on this machine it is set by Docker, Ollama, ClipVault,
/// ShotVault, uTorrent Web and SensibleSideButtons in `/Applications`, and by Mission
/// Control, Siri, Time Machine, Screenshot and System Information in
/// `/System/Applications` — twelve apps a launcher plainly should offer.
///
/// But `/System/Library/CoreServices` is 89 agents out of 116 (NetAuthAgent,
/// BluetoothUIServer, TipsSpotlightHandler...), which is noise nobody launches. So the
/// flag is only disqualifying in the system service directories, where it happens to
/// correlate with junk, and is ignored in the user-facing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agents {
    Include,
    Exclude,
}

/// Scans `roots` for launchable bundles. Unreadable directories and malformed bundles
/// are skipped rather than reported: one bad app must not cost the user their launcher.
pub fn scan(roots: &[PathBuf]) -> Vec<AppEntry> {
    let mut out = Vec::new();
    for root in roots {
        let agents = agent_policy(root);
        scan_dir(root, agents, 0, &mut out);
    }
    out
}

/// `/System/Library/CoreServices` and friends hold system agents; everywhere else holds
/// things people install and launch. See [`Agents`] for why this is a per-directory
/// decision rather than one global rule.
fn agent_policy(root: &Path) -> Agents {
    if root.starts_with("/System/Library/CoreServices") {
        Agents::Exclude
    } else {
        Agents::Include
    }
}

fn scan_dir(dir: &Path, agents: Agents, depth: usize, out: &mut Vec<AppEntry>) {
    // A configured path that does not exist is not an error — `~/Applications` is
    // absent on plenty of machines, and `/Applications/Utilities` is empty on this one.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // `fs::metadata`, not `entry.file_type()`. The latter comes straight from
        // `readdir` and does not follow symlinks — and `/Applications/Safari.app` is a
        // symlink into `/System/Cryptexes/App`. Trusting the dirent would silently drop
        // Safari from the index, which is the kind of bug you notice six months later.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }

        if is_bundle(&path) {
            if let Some(app) = read_bundle(&path, agents) {
                out.push(app);
            }
            // Never descend into a bundle. There are 105 nested `.app`s under
            // `Contents/` across `/Applications` alone — Electron renderer helpers,
            // login items, crash reporters, Xcode's bundled tools — and not one of them
            // is something a person launches by name.
            continue;
        }

        if depth < MAX_DEPTH {
            scan_dir(&path, agents, depth + 1, out);
        }
    }
}

fn is_bundle(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
}

fn read_bundle(bundle_path: &Path, agents: Agents) -> Option<AppEntry> {
    let info = read_info_plist(bundle_path).ok()?;
    entry_from_info_plist(bundle_path, &info, agents)
}

/// Builds an entry from an already-parsed `Info.plist`.
///
/// Note what is deliberately *not* tested here. `CFBundlePackageType == "APPL"` looks
/// like the definitive launchability marker and is not: on this machine Passwords.app
/// is `XPC!`, Finder is `FNDR`, Netflix and ManagedClient are `AAPL`, and three bundles
/// omit the key entirely. `CFBundleExecutable` is likewise absent from Netflix.app and
/// PassViewer.app. Either check would drop real apps, so the predicate is simply: a
/// directory named `*.app`, with a readable `Info.plist`, that is not an agent.
pub fn entry_from_info_plist(
    bundle_path: &Path,
    info: &Dictionary,
    agents: Agents,
) -> Option<AppEntry> {
    if is_agent(info, agents) {
        return None;
    }
    let name = display_name(bundle_path, info)?;
    Some(AppEntry::new(name, bundle_path.to_path_buf()))
}

/// Why a bundle could not be read. Never fatal: a malformed bundle is skipped.
#[derive(Debug)]
pub enum BundleError {
    Plist(plist::Error),
    NotADictionary,
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plist(e) => write!(f, "could not read Info.plist: {e}"),
            Self::NotADictionary => write!(f, "Info.plist root is not a dictionary"),
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plist(e) => Some(e),
            Self::NotADictionary => None,
        }
    }
}

/// Reads and parses `<bundle>/Contents/Info.plist`.
///
/// The `plist` crate sniffs the encoding from the file header, which matters: of the
/// 216 bundles in the default scan paths here, 8 are binary and 209 are XML.
pub fn read_info_plist(bundle_path: &Path) -> Result<Dictionary, BundleError> {
    let path = bundle_path.join("Contents/Info.plist");
    let value = Value::from_file(path).map_err(BundleError::Plist)?;
    value.into_dictionary().ok_or(BundleError::NotADictionary)
}

/// `CFBundleDisplayName` -> `CFBundleName` -> the bundle filename without `.app`.
///
/// All three steps are load-bearing on this machine: seven `/Applications` bundles have
/// only `CFBundleName`, and SensibleSideButtons.app has neither key.
///
/// Localised display names are not resolved through `InfoPlist.strings`. Research found
/// zero bundles across `/Applications` and `/System/Applications` that localise
/// `CFBundleDisplayName` that way, so the lookup would be pure cost. Worth revisiting if
/// a real app ever shows up with a raw key as its name.
fn display_name(bundle_path: &Path, info: &Dictionary) -> Option<String> {
    let from_key = |key: &str| {
        info.get(key)
            .and_then(Value::as_string)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };

    from_key("CFBundleDisplayName")
        .or_else(|| from_key("CFBundleName"))
        .or_else(|| {
            bundle_path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

/// `LSBackgroundOnly` disqualifies a bundle everywhere — it declares no UI at all, so
/// there is nothing to bring to the front. `LSUIElement` only disqualifies where the
/// [`Agents`] policy says so.
fn is_agent(info: &Dictionary, agents: Agents) -> bool {
    if info.get("LSBackgroundOnly").is_some_and(truthy) {
        return true;
    }
    agents == Agents::Exclude && info.get("LSUIElement").is_some_and(truthy)
}

/// Real `Info.plist` files spell these flags inconsistently. Across the scan paths on
/// this machine the wild forms are `<true/>` (80 times), `<string>1</string>` (12),
/// `<string>YES</string>` (2) and `<string>0</string>` for false (1). No integer form
/// turned up, but accepting it costs nothing and the alternative is misclassifying
/// Time Machine and Mission Control.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Boolean(b) => *b,
        Value::Integer(i) => i.as_signed().is_some_and(|n| n != 0),
        Value::String(s) => {
            let s = s.trim();
            s.eq_ignore_ascii_case("1")
                || s.eq_ignore_ascii_case("yes")
                || s.eq_ignore_ascii_case("true")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, Value)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.insert((*k).to_owned(), v.clone());
        }
        d
    }

    fn path() -> PathBuf {
        PathBuf::from("/Applications/Slack.app")
    }

    fn entry(info: &Dictionary) -> Option<AppEntry> {
        entry_from_info_plist(&path(), info, Agents::Include)
    }

    #[test]
    fn display_name_prefers_display_then_name_then_filename() {
        let both = dict(&[
            ("CFBundleDisplayName", Value::String("Display".into())),
            ("CFBundleName", Value::String("Name".into())),
        ]);
        assert_eq!(display_name(&path(), &both).as_deref(), Some("Display"));

        let name_only = dict(&[("CFBundleName", Value::String("Name".into()))]);
        assert_eq!(display_name(&path(), &name_only).as_deref(), Some("Name"));

        assert_eq!(
            display_name(&path(), &Dictionary::new()).as_deref(),
            Some("Slack")
        );
    }

    #[test]
    fn blank_display_name_falls_through_rather_than_winning() {
        let blank = dict(&[
            ("CFBundleDisplayName", Value::String("   ".into())),
            ("CFBundleName", Value::String("Name".into())),
        ]);
        assert_eq!(display_name(&path(), &blank).as_deref(), Some("Name"));
    }

    #[test]
    fn non_string_display_name_falls_through() {
        let wrong_type = dict(&[
            ("CFBundleDisplayName", Value::Integer(7.into())),
            ("CFBundleName", Value::String("Name".into())),
        ]);
        assert_eq!(display_name(&path(), &wrong_type).as_deref(), Some("Name"));
    }

    #[test]
    fn background_only_is_skipped_even_where_agents_are_included() {
        for value in [
            Value::Boolean(true),
            Value::String("1".into()),
            Value::String("YES".into()),
            Value::Integer(1.into()),
        ] {
            let info = dict(&[("LSBackgroundOnly", value.clone())]);
            assert!(entry(&info).is_none(), "should skip {value:?}");
        }
    }

    #[test]
    fn ui_element_is_kept_in_user_directories_and_dropped_in_system_ones() {
        // Docker, Ollama, ClipVault and friends all look like this.
        let info = dict(&[("LSUIElement", Value::Boolean(true))]);
        assert!(
            entry_from_info_plist(&path(), &info, Agents::Include).is_some(),
            "a menu-bar app is still launchable"
        );
        assert!(entry_from_info_plist(&path(), &info, Agents::Exclude).is_none());
    }

    #[test]
    fn agent_flags_are_recognised_however_they_are_spelled() {
        for value in [
            Value::Boolean(true),
            Value::String("1".into()),
            Value::String("YES".into()),
            Value::String("true".into()),
            Value::Integer(1.into()),
        ] {
            let info = dict(&[("LSUIElement", value.clone())]);
            assert!(
                entry_from_info_plist(&path(), &info, Agents::Exclude).is_none(),
                "should recognise agent declared as {value:?}"
            );
        }
    }

    #[test]
    fn falsy_flags_do_not_skip() {
        // /System/Library/CoreServices/System Events.app really does spell it "0".
        for value in [
            Value::Boolean(false),
            Value::String("0".into()),
            Value::String("NO".into()),
            Value::Integer(0.into()),
        ] {
            let info = dict(&[
                ("LSUIElement", value.clone()),
                ("LSBackgroundOnly", value.clone()),
            ]);
            assert!(
                entry_from_info_plist(&path(), &info, Agents::Exclude).is_some(),
                "should keep app with flags = {value:?}"
            );
        }
    }

    #[test]
    fn a_plain_app_becomes_an_entry_carrying_its_path() {
        let info = dict(&[("CFBundleDisplayName", Value::String("Slack".into()))]);
        let app = entry(&info).expect("launchable");
        assert_eq!(app.name, "Slack");
        assert_eq!(app.path, path());
    }

    #[test]
    fn the_agent_policy_is_scoped_to_the_system_service_directories() {
        assert_eq!(agent_policy(Path::new("/Applications")), Agents::Include);
        assert_eq!(
            agent_policy(Path::new("/System/Applications")),
            Agents::Include
        );
        assert_eq!(
            agent_policy(Path::new("/System/Library/CoreServices")),
            Agents::Exclude
        );
    }

    // --- directory walk -----------------------------------------------------

    /// A scratch directory that cleans itself up. Hand-rolled rather than pulling in
    /// `tempfile` for one test module.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("blindspot-test-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes a minimal bundle at `path` with the given Info.plist keys.
    fn make_bundle(path: &Path, pairs: &[(&str, Value)]) {
        std::fs::create_dir_all(path.join("Contents")).expect("create bundle");
        let mut info = dict(pairs);
        if !info.contains_key("CFBundleName") {
            info.insert("CFBundleName".to_owned(), Value::String("Unnamed".into()));
        }
        Value::Dictionary(info)
            .to_file_xml(path.join("Contents/Info.plist"))
            .expect("write Info.plist");
    }

    fn names(entries: &[AppEntry]) -> Vec<&str> {
        let mut v: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn scan_finds_bundles_at_the_root_and_one_level_down() {
        let dir = TempDir::new("depth");
        make_bundle(
            &dir.path().join("Top.app"),
            &[("CFBundleName", Value::String("Top".into()))],
        );
        // The `/Applications/Cisco/*.app` shape.
        make_bundle(
            &dir.path().join("Vendor/Nested.app"),
            &[("CFBundleName", Value::String("Nested".into()))],
        );
        // Two levels down is out of range.
        make_bundle(
            &dir.path().join("Vendor/Deeper/TooDeep.app"),
            &[("CFBundleName", Value::String("TooDeep".into()))],
        );

        let found = scan(&[dir.path().to_path_buf()]);
        assert_eq!(names(&found), ["Nested", "Top"]);
    }

    #[test]
    fn scan_never_descends_into_a_bundle() {
        let dir = TempDir::new("nested");
        let host = dir.path().join("Host.app");
        make_bundle(&host, &[("CFBundleName", Value::String("Host".into()))]);
        // The Electron helper shape: 105 of these exist under /Applications.
        make_bundle(
            &host.join("Contents/Frameworks/Host Helper (GPU).app"),
            &[("CFBundleName", Value::String("Host Helper (GPU)".into()))],
        );
        make_bundle(
            &host.join("Contents/Library/LoginItems/Launcher.app"),
            &[("CFBundleName", Value::String("Launcher".into()))],
        );

        let found = scan(&[dir.path().to_path_buf()]);
        assert_eq!(names(&found), ["Host"]);
    }

    #[test]
    fn scan_follows_a_symlinked_bundle() {
        // Exactly the Safari case: /Applications/Safari.app is a symlink into
        // /System/Cryptexes/App, and a `readdir`-only file type check misses it.
        let real = TempDir::new("symlink-target");
        let dir = TempDir::new("symlink");
        let target = real.path().join("Real.app");
        make_bundle(&target, &[("CFBundleName", Value::String("Real".into()))]);
        std::os::unix::fs::symlink(&target, dir.path().join("Linked.app")).expect("symlink");

        let found = scan(&[dir.path().to_path_buf()]);
        assert_eq!(names(&found), ["Real"]);
        assert_eq!(found[0].path, dir.path().join("Linked.app"));
    }

    #[test]
    fn scan_skips_bundles_without_a_readable_info_plist() {
        let dir = TempDir::new("malformed");
        std::fs::create_dir_all(dir.path().join("Empty.app/Contents")).expect("mkdir");
        std::fs::write(dir.path().join("Broken.app"), b"not a directory").expect("write");
        make_bundle(
            &dir.path().join("Good.app"),
            &[("CFBundleName", Value::String("Good".into()))],
        );

        assert_eq!(names(&scan(&[dir.path().to_path_buf()])), ["Good"]);
    }

    #[test]
    fn scan_of_a_missing_directory_is_empty_rather_than_an_error() {
        assert!(scan(&[PathBuf::from("/nonexistent/blindspot/apps")]).is_empty());
        assert!(scan(&[]).is_empty());
    }
}
