//! Ranking for `?` queries, where apps and files share one list.
//!
//! `nucleo` is built to decide *whether* something matches, not which match is best. For
//! `?desktop` it scored the Desktop folder, "Podman Desktop", a crash-reporter plist and
//! two Docker log files identically — 192 each — so `mdfind`'s arbitrary output order
//! decided, and 44 of its first 50 results were `Library/` internals. This orders
//! candidates the way a person would, keeping nucleo's score only as the last tie-break.

use std::cmp::Reverse;
use std::path::{Component, Path};

/// How well a name matches, worst to best, so the derived ordering reads naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Matched only as a scattered subsequence — `dsk` in "Desktop".
    Fuzzy,
    /// Contained somewhere — `top` in "Desktop".
    Substring,
    /// A word inside the name starts with it — `desktop` in "Podman Desktop".
    WordStart,
    /// The name starts with it — `desk` in "Desktop Pictures".
    Prefix,
    /// The whole name, or a file's name without its extension — `notes` for "notes.md".
    Exact,
}

pub fn tier(name: &str, query: &str) -> Tier {
    let name = name.to_lowercase();
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Tier::Fuzzy;
    }
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    if name == query || stem == query {
        Tier::Exact
    } else if name.starts_with(&query) {
        Tier::Prefix
    } else if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(&query))
    {
        Tier::WordStart
    } else if name.contains(&query) {
        Tier::Substring
    } else {
        Tier::Fuzzy
    }
}

/// Where a candidate lives, worst to best.
///
/// Three levels rather than a noise flag, because "not system internals" was not enough:
/// `?desktop` ranked three Unreal Engine source files from `/Users/Shared/Epic Games` above
/// everything but the folder itself. They are not noise in general, but they are not
/// yours, and they must not outrank anything that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// System, library and tooling internals. Shown only when nothing better matches.
    Noise,
    /// Real files that are not yours — `/Users/Shared`, SDK trees under `/Applications`.
    Elsewhere,
    /// Apps, your home folder, and external drives.
    Yours,
}

/// A candidate's sort key. Compared field by field in declaration order, greater is
/// better — so each field only matters when everything above it ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// First, and deliberately above the match tier: an exact name deep inside
    /// `~/Library/Containers` or an engine SDK must not beat `notes-2024.md` in your home.
    place: Place,
    /// Exact, prefix or start-of-word, as against merely contained or fuzzy. A band rather
    /// than the tier itself, so the fields below can reorder *within* strong matches
    /// without ever letting a weak one through.
    strong: bool,
    /// Launched or opened through blindspot before. An app you open daily outranks a
    /// same-strength file you have never touched.
    used: bool,
    /// An app, or something directly inside your home folder. Above the exact tier on
    /// purpose: `?doc` put a stock-price file named `DOC.csv`, seven folders deep, ahead
    /// of `~/Documents`, because an exact name beat a prefix outright. Among strong
    /// matches, your top-level things are almost always what the query meant.
    top_level: bool,
    tier: Tier,
    /// A file in your home folder. At equal tier, files beat apps: typing `?` is asking
    /// for files, and apps are one keystroke away without it.
    user_file: bool,
    /// Fewer folders between you and it.
    depth: Reverse<usize>,
    /// Less unrelated text around the match.
    length: Reverse<usize>,
    /// nucleo's score, frecency-boosted. Only ever breaks a tie.
    fuzzy: u32,
}

pub struct Candidate<'a> {
    pub name: &'a str,
    pub path: &'a Path,
    pub is_app: bool,
    pub fuzzy: u32,
    pub used: bool,
}

pub fn key(candidate: &Candidate<'_>, query: &str, home: Option<&Path>) -> Key {
    let noise = !candidate.is_app && is_noise(candidate.path);
    let under_home = home.and_then(|h| candidate.path.strip_prefix(h).ok());
    let place = if candidate.is_app {
        Place::Yours
    } else if noise {
        Place::Noise
    } else if under_home.is_some() || candidate.path.starts_with("/Volumes") {
        Place::Yours
    } else {
        Place::Elsewhere
    };
    let tier = tier(candidate.name, query);
    Key {
        place,
        strong: tier >= Tier::WordStart,
        used: candidate.used,
        top_level: candidate.is_app
            || under_home.is_some_and(|rest| rest.components().count() == 1),
        tier,
        user_file: !candidate.is_app && !noise && under_home.is_some(),
        depth: Reverse(if candidate.is_app {
            0
        } else {
            under_home.map_or_else(
                || candidate.path.components().count(),
                |rest| rest.components().count(),
            )
        }),
        length: Reverse(candidate.name.chars().count()),
        fuzzy: candidate.fuzzy,
    }
}

/// Folders whose contents are machinery rather than anything a person opens by name.
///
/// `target` is Cargo's and Maven's build output — a folder by that name is overwhelmingly
/// generated on a developer machine, and `?blindspot` surfaced incremental-compilation
/// directories from inside it.
const TOOLING: &[&str] = &[
    "node_modules",
    "DerivedData",
    "__pycache__",
    "Pods",
    "target",
    // Dependency trees that real runs surfaced: Python virtualenvs put numpy's `doc/`
    // above `~/Documents` for `?doc`, and CMake's `_deps` put raylib sources above
    // everything but the folder itself for `?desktop`.
    "venv",
    "site-packages",
    "_deps",
    "vendor",
    "bower_components",
];

/// Root-level trees that belong to the system.
const SYSTEM_ROOTS: &[&str] = &[
    "System", "usr", "private", "opt", "bin", "sbin", "cores", "dev",
];

/// Bundle-like containers. Anything *inside* one is an implementation detail of it.
const CONTAINERS: &[&str] = &[".app", ".framework", ".bundle", ".plugin", ".appex", ".xpc"];

/// Demoted, never hidden: a result here still appears if nothing better matches, which
/// is what `?Desktop Pictures` needs. It just cannot outrank anything of yours.
pub fn is_noise(path: &Path) -> bool {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();

    if parts
        .first()
        .is_some_and(|root| SYSTEM_ROOTS.contains(root))
    {
        return true;
    }
    // Go's module cache is the one major package cache that is not a dotfolder, so the
    // hidden-folder rule below misses it — and `?doc` ranked a vendored `doc.go` first.
    if parts.windows(2).any(|w| w == ["go", "pkg"]) {
        return true;
    }
    let last = parts.len().saturating_sub(1);
    parts.iter().enumerate().any(|(i, part)| {
        *part == "Library"
            || TOOLING.contains(part)
            // Hidden folders on the way — `.git`, `.cache`, `.cargo`, blindspot's own
            // `.local` data. The final component is exempt: a dotfile you named is yours.
            || (i < last && part.starts_with('.'))
            || (i < last && CONTAINERS.iter().any(|ext| part.ends_with(ext)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const HOME: &str = "/Users/seif";

    fn rank(query: &str, items: &[(&str, &str, bool, bool)]) -> Vec<String> {
        let home = Path::new(HOME);
        let paths: Vec<PathBuf> = items.iter().map(|i| PathBuf::from(i.1)).collect();
        let mut keyed: Vec<(Key, &str)> = items
            .iter()
            .zip(&paths)
            .map(|(&(name, _, is_app, used), path)| {
                let c = Candidate {
                    name,
                    path,
                    is_app,
                    fuzzy: 192,
                    used,
                };
                (key(&c, query, Some(home)), name)
            })
            .collect();
        keyed.sort_by_key(|k| Reverse(k.0));
        keyed.into_iter().map(|(_, n)| n.to_owned()).collect()
    }

    #[test]
    fn the_desktop_case_that_prompted_this() {
        // The real `?desktop` results, every one of which nucleo scored 192.
        let got = rank(
            "desktop",
            &[
                (
                    "Podman Desktop",
                    "/Applications/Podman Desktop.app",
                    true,
                    false,
                ),
                (
                    "DesktopPlists",
                    "/System/Library/PrivateFrameworks/X.framework/Resources/DesktopPlists",
                    false,
                    false,
                ),
                (
                    "Docker Desktop.stdout.log",
                    "/Users/seif/Library/Containers/d/Data/log/Docker Desktop.stdout.log",
                    false,
                    false,
                ),
                (
                    "Desktop Pictures",
                    "/System/Library/Desktop Pictures",
                    false,
                    false,
                ),
                ("Desktop", "/Users/seif/Desktop", false, false),
            ],
        );
        assert_eq!(got[0], "Desktop", "the folder itself, first");
        assert_eq!(
            got[1], "Podman Desktop",
            "then the app — ranked, not first by kind"
        );
        assert!(
            got[2..]
                .iter()
                .all(|n| n != "Desktop" && n != "Podman Desktop"),
            "system internals come last: {got:?}"
        );
    }

    #[test]
    fn tiers_order_exact_prefix_word_substring_fuzzy() {
        assert_eq!(tier("Desktop", "desktop"), Tier::Exact);
        assert_eq!(tier("notes.md", "notes"), Tier::Exact, "extension ignored");
        assert_eq!(tier("Desktop Pictures", "desk"), Tier::Prefix);
        assert_eq!(tier("Podman Desktop", "desktop"), Tier::WordStart);
        assert_eq!(tier("my-report.pdf", "report"), Tier::WordStart);
        assert_eq!(tier("Desktop", "top"), Tier::Substring);
        assert_eq!(tier("Desktop", "dsk"), Tier::Fuzzy);
        assert!(Tier::Exact > Tier::Prefix && Tier::Prefix > Tier::WordStart);
    }

    #[test]
    fn noise_loses_even_with_an_exact_name() {
        let got = rank(
            "notes",
            &[
                (
                    "notes",
                    "/Users/seif/Library/Application Support/X/notes",
                    false,
                    false,
                ),
                (
                    "notes-2024.md",
                    "/Users/seif/Documents/notes-2024.md",
                    false,
                    false,
                ),
            ],
        );
        assert_eq!(got[0], "notes-2024.md");
    }

    #[test]
    fn something_you_use_beats_an_unused_file_at_the_same_tier() {
        let got = rank(
            "term",
            &[
                (
                    "terminal-notes.md",
                    "/Users/seif/terminal-notes.md",
                    false,
                    false,
                ),
                (
                    "Terminal",
                    "/System/Applications/Utilities/Terminal.app",
                    true,
                    true,
                ),
            ],
        );
        assert_eq!(got[0], "Terminal", "launched daily");
    }

    #[test]
    fn at_equal_tier_files_beat_apps_because_the_query_asked_for_files() {
        let got = rank(
            "docker",
            &[
                ("Docker", "/Applications/Docker.app", true, false),
                ("docker", "/Users/seif/docker", false, false),
            ],
        );
        assert_eq!(got[0], "docker");
    }

    #[test]
    fn shallower_beats_deeper() {
        // Same name at two depths, so only the path can decide — compare paths, not names.
        let home = Path::new(HOME);
        let deep = PathBuf::from("/Users/seif/archive/2025/old/blindspot");
        let shallow = PathBuf::from("/Users/seif/blindspot");
        let key_for = |path: &Path| {
            let c = Candidate {
                name: "blindspot",
                path,
                is_app: false,
                fuzzy: 192,
                used: false,
            };
            key(&c, "blindspot", Some(home))
        };
        assert!(
            key_for(&shallow) > key_for(&deep),
            "~/blindspot must outrank a buried copy"
        );
    }

    #[test]
    fn someone_elses_exact_match_loses_to_your_partial_one() {
        // The real `?desktop` results: Unreal Engine files under /Users/Shared.
        let got = rank(
            "desktop",
            &[
                (
                    "Desktop",
                    "/Users/Shared/Epic Games/UE_5.8/Engine/Content/Slate/Desktop",
                    false,
                    false,
                ),
                (
                    "desktop-notes.md",
                    "/Users/seif/desktop-notes.md",
                    false,
                    false,
                ),
                ("Desktop", "/Users/seif/Desktop", false, false),
            ],
        );
        assert_eq!(
            got[..2],
            ["Desktop", "desktop-notes.md"],
            "both of yours first: {got:?}"
        );
    }

    #[test]
    fn external_drives_count_as_yours() {
        let home = Path::new(HOME);
        let c = |path: &'static str| Candidate {
            name: "photos",
            path: Path::new(path),
            is_app: false,
            fuzzy: 1,
            used: false,
        };
        let drive = key(&c("/Volumes/Backup/photos"), "photos", Some(home));
        let shared = key(&c("/Users/Shared/photos"), "photos", Some(home));
        assert!(drive > shared);
    }

    #[test]
    fn a_deep_exact_match_loses_to_a_top_level_prefix() {
        // The real `?doc` result: DOC.csv (a ticker's price data) above ~/Documents.
        let got = rank(
            "doc",
            &[
                (
                    "DOC.csv",
                    "/Users/seif/gl/research/exp004/data/prices/DOC.csv",
                    false,
                    false,
                ),
                ("Documents", "/Users/seif/Documents", false, false),
            ],
        );
        assert_eq!(got[0], "Documents");
    }

    #[test]
    fn a_weak_top_level_match_still_loses_to_a_strong_deep_one() {
        // The band must not let a merely-contained name ride its location to the top.
        let got = rank(
            "report",
            &[
                (
                    "old-misreporting",
                    "/Users/seif/old-misreporting",
                    false,
                    false,
                ),
                (
                    "report.pdf",
                    "/Users/seif/Documents/work/report.pdf",
                    false,
                    false,
                ),
            ],
        );
        assert_eq!(got[0], "report.pdf");
    }

    #[test]
    fn what_counts_as_noise() {
        for noisy in [
            "/System/Library/Desktop Pictures",
            "/Library/Fonts/x.ttf",
            "/Users/seif/Library/Caches/x",
            "/Applications/Xcode.app/Contents/Resources/x.plist",
            "/Users/seif/proj/node_modules/left-pad/index.js",
            "/Users/seif/proj/.git/HEAD",
            "/Users/seif/.cargo/registry/x",
            "/usr/share/vim/desktop.vim",
            "/Users/seif/X.framework/Resources/y",
            "/Users/seif/go/pkg/mod/github.com/gorilla/websocket@v1.5.3/doc.go",
            "/Users/seif/blindspot/core/target/debug/incremental/x",
            "/Users/seif/bot/venv/lib/python3.12/site-packages/numpy/doc",
            "/Users/seif/caesar/build/_deps/raylib-src/src/rcore_desktop_sdl.c",
        ] {
            assert!(is_noise(Path::new(noisy)), "{noisy} should be noise");
        }
        for mine in [
            "/Users/seif/Desktop",
            "/Users/seif/Documents/report.pdf",
            "/Users/seif/.zshrc",
            "/Volumes/Backup/photos",
            "/Applications/Podman Desktop.app",
        ] {
            assert!(!is_noise(Path::new(mine)), "{mine} should not be noise");
        }
    }
}
