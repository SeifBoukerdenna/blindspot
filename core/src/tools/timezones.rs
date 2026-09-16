//! Time zones: `time in paris`, `paris time`, `3pm montreal in tokyo`, `9am to london`,
//! `15:00 utc in montreal`.
//!
//! Rules come from the TZif files macOS keeps in /usr/share/zoneinfo, so daylight saving is exact
//! without bundling a database or adding a dependency. Each file's POSIX footer rule covers
//! instants after its last listed transition. Places are the IANA zone cities (Tokyo, Paris,
//! New York), plus a short table of common cities, countries and abbreviations that have no zone
//! of their own (Montreal, San Francisco, PST). An unknown place produces nothing, so a search is
//! never replaced by a guess.
//!
//! `3pm in tokyo` reads 3pm as Tokyo's time and shows yours; `3pm to tokyo` reads it as yours and
//! shows Tokyo's. Every conversion shows both clocks, so either reading is on screen.

use super::ToolRow;
use std::path::Path;
use std::sync::OnceLock;

const ZONEINFO: &str = "/usr/share/zoneinfo";
const DAY: i64 = 86_400;

/// The detector: a cheap shape check first, since every launcher query reaches it.
pub(super) fn evaluate(query: &str) -> Option<Vec<ToolRow>> {
    let trimmed = query.trim_start();
    let lower = trimmed.to_lowercase();
    let plausible = trimmed.starts_with(|c: char| c.is_ascii_digit())
        || lower.contains("time")
        || lower.starts_with("noon")
        || lower.starts_with("midnight");
    if !plausible {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    rows(query, i64::try_from(now).ok()?, &local_zone())
}

pub(super) fn rows(query: &str, now: i64, local: &str) -> Option<Vec<ToolRow>> {
    let text = query
        .trim()
        .trim_end_matches(['?', '.', '!'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if text.len() < 6 || text.len() > 80 {
        return None;
    }
    let you = || Place {
        zone: local.to_owned(),
        name: "you".to_owned(),
    };
    for prefix in [
        "what time is it in ",
        "what's the time in ",
        "whats the time in ",
        "current time in ",
        "time now in ",
        "time in ",
        "time at ",
        "now in ",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return current(&place(rest, local)?, now, &you());
        }
    }
    if let Some(rest) = text.strip_suffix(" time")
        && let Some(found) = place(rest, local)
    {
        return current(&found, now, &you());
    }
    let (seconds, rest) = leading_clock(&text)?;
    let (from, to) = if let Some(target) = rest.strip_prefix("in ") {
        (place(target, local)?, you())
    } else if let Some(target) = rest.strip_prefix("to ") {
        (you(), place(target, local)?)
    } else if let Some((source, target)) =
        rest.split_once(" in ").or_else(|| rest.split_once(" to "))
    {
        (place(source, local)?, place(target, local)?)
    } else {
        (place(rest, local)?, you())
    };
    convert(seconds, &from, &to, now)
}

pub(super) fn local_zone() -> String {
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| {
            path.to_string_lossy()
                .split_once("zoneinfo/")
                .map(|(_, id)| id.to_owned())
        })
        .unwrap_or_else(|| "UTC".to_owned())
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

struct Place {
    zone: String,
    /// What the rows call it: "Tokyo", "PST", or "you" for the local zone.
    name: String,
}

fn current(place: &Place, now: i64, you: &Place) -> Option<Vec<ToolRow>> {
    let there = zone(&place.zone)?;
    let (offset, abbreviation) = there.at(now);
    let mut rows = vec![
        ToolRow::new(clock(now + i64::from(offset)), label(&place.name)),
        ToolRow::new(
            format!("{abbreviation} · UTC{}", utc_offset(offset)),
            "zone",
        ),
    ];
    if place.name != "you"
        && let Some(here) = zone(&you.zone)
    {
        rows.push(ToolRow::new(
            difference(offset - here.at(now).0, &place.name, "you"),
            "difference",
        ));
    }
    Some(rows)
}

fn convert(seconds: i64, from: &Place, to: &Place, now: i64) -> Option<Vec<ToolRow>> {
    let (source, target) = (zone(&from.zone)?, zone(&to.zone)?);
    // The clock time on today's date where it was given, found in UTC in two steps so a guess
    // made with the wrong side of a daylight-saving change corrects itself.
    let day = (now + i64::from(source.at(now).0)).div_euclid(DAY);
    let wall = day * DAY + seconds;
    let guess = wall - i64::from(source.at(wall).0);
    let instant = wall - i64::from(source.at(guess).0);
    let (from_offset, to_offset) = (source.at(instant).0, target.at(instant).0);
    Some(vec![
        ToolRow::new(clock(instant + i64::from(to_offset)), label(&to.name)),
        ToolRow::new(clock(instant + i64::from(from_offset)), label(&from.name)),
        ToolRow::new(
            difference(to_offset - from_offset, &to.name, &from.name),
            "difference",
        ),
    ])
}

fn label(name: &str) -> String {
    if name == "you" {
        "Your time".to_owned()
    } else {
        name.to_owned()
    }
}

/// "Tokyo is 13 h ahead of Montreal", "You are 5 h 30 min behind Delhi", "Same time as you".
fn difference(seconds: i32, first: &str, second: &str) -> String {
    let object = if second == "you" {
        "you".to_owned()
    } else {
        second.to_owned()
    };
    if seconds == 0 {
        return format!("Same time as {object}");
    }
    let subject = if first == "you" {
        "You are".to_owned()
    } else {
        format!("{first} is")
    };
    let direction = if seconds > 0 { "ahead of" } else { "behind" };
    let size = seconds.unsigned_abs();
    let span = match (size / 3600, size % 3600 / 60) {
        (hours, 0) => format!("{hours} h"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    format!("{subject} {span} {direction} {object}")
}

/// `4:00 AM · Wed` for seconds since the epoch in local time.
fn clock(local: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let seconds = local.rem_euclid(DAY);
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    let (shown, meridiem) = match hours {
        0 => (12, "AM"),
        1..=11 => (hours, "AM"),
        12 => (12, "PM"),
        _ => (hours - 12, "PM"),
    };
    let weekday = usize::try_from(local.div_euclid(DAY).rem_euclid(7))
        .ok()
        .and_then(|index| WEEKDAYS.get(index))
        .copied()
        .unwrap_or("");
    format!("{shown}:{minutes:02} {meridiem} · {weekday}")
}

fn utc_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let size = seconds.unsigned_abs();
    match (size / 3600, size % 3600 / 60) {
        (hours, 0) => format!("{sign}{hours}"),
        (hours, minutes) => format!("{sign}{hours}:{minutes:02}"),
    }
}

/// `3pm`, `3 pm`, `3:30pm`, `3.30 p.m.`, `15:00`, `15h`, `15h30`, `noon`, `midnight`, and what
/// follows. A bare number is never a clock, so `4 in a row` stays a search.
fn leading_clock(text: &str) -> Option<(i64, &str)> {
    let (first, mut rest) = text.split_once(' ').unwrap_or((text, ""));
    let mut written = first.to_owned();
    for suffix in ["am", "pm", "a.m.", "p.m."] {
        if rest == suffix
            || rest
                .strip_prefix(suffix)
                .is_some_and(|after| after.starts_with(' '))
        {
            written.push_str(suffix);
            rest = rest[suffix.len()..].trim_start();
            break;
        }
    }
    Some((clock_seconds(&written)?, rest))
}

fn clock_seconds(text: &str) -> Option<i64> {
    match text {
        "noon" => return Some(12 * 3600),
        "midnight" => return Some(0),
        _ => {}
    }
    let (body, afternoon) = if let Some(body) = text
        .strip_suffix("am")
        .or_else(|| text.strip_suffix("a.m."))
    {
        (body, Some(false))
    } else if let Some(body) = text
        .strip_suffix("pm")
        .or_else(|| text.strip_suffix("p.m."))
    {
        (body, Some(true))
    } else {
        (text, None)
    };
    let separated = body.contains([':', 'h', '.']);
    let (hours, minutes) = match body.split_once([':', 'h', '.']) {
        Some((hours, "")) => (hours, "0"),
        Some(parts) => parts,
        None => (body, "0"),
    };
    let digits = |part: &str| {
        !part.is_empty() && part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(hours) || !digits(minutes) {
        return None;
    }
    let (hours, minutes): (i64, i64) = (hours.parse().ok()?, minutes.parse().ok()?);
    if minutes > 59 {
        return None;
    }
    match afternoon {
        Some(pm) if (1..=12).contains(&hours) => {
            Some((hours % 12 + if pm { 12 } else { 0 }) * 3600 + minutes * 60)
        }
        None if separated && hours < 24 => Some(hours * 3600 + minutes * 60),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Places
// ---------------------------------------------------------------------------

/// Cities, countries and abbreviations people type that are not zone names themselves.
const ALIASES: &[(&str, &str)] = &[
    ("utc", "UTC"),
    ("gmt", "UTC"),
    ("et", "America/New_York"),
    ("est", "America/New_York"),
    ("edt", "America/New_York"),
    ("eastern", "America/New_York"),
    ("ct", "America/Chicago"),
    ("cst", "America/Chicago"),
    ("cdt", "America/Chicago"),
    ("central", "America/Chicago"),
    ("mt", "America/Denver"),
    ("mst", "America/Denver"),
    ("mdt", "America/Denver"),
    ("mountain", "America/Denver"),
    ("pt", "America/Los_Angeles"),
    ("pst", "America/Los_Angeles"),
    ("pdt", "America/Los_Angeles"),
    ("pacific", "America/Los_Angeles"),
    ("cet", "Europe/Paris"),
    ("cest", "Europe/Paris"),
    ("bst", "Europe/London"),
    ("ist", "Asia/Kolkata"),
    ("jst", "Asia/Tokyo"),
    ("kst", "Asia/Seoul"),
    ("aest", "Australia/Sydney"),
    ("aedt", "Australia/Sydney"),
    ("hkt", "Asia/Hong_Kong"),
    ("sgt", "Asia/Singapore"),
    ("montreal", "America/Toronto"),
    ("montréal", "America/Toronto"),
    ("quebec", "America/Toronto"),
    ("quebec city", "America/Toronto"),
    ("ottawa", "America/Toronto"),
    ("nyc", "America/New_York"),
    ("ny", "America/New_York"),
    ("new york city", "America/New_York"),
    ("boston", "America/New_York"),
    ("washington", "America/New_York"),
    ("washington dc", "America/New_York"),
    ("dc", "America/New_York"),
    ("miami", "America/New_York"),
    ("atlanta", "America/New_York"),
    ("philadelphia", "America/New_York"),
    ("san francisco", "America/Los_Angeles"),
    ("sf", "America/Los_Angeles"),
    ("la", "America/Los_Angeles"),
    ("seattle", "America/Los_Angeles"),
    ("portland", "America/Los_Angeles"),
    ("san jose", "America/Los_Angeles"),
    ("silicon valley", "America/Los_Angeles"),
    ("las vegas", "America/Los_Angeles"),
    ("austin", "America/Chicago"),
    ("dallas", "America/Chicago"),
    ("houston", "America/Chicago"),
    ("new orleans", "America/Chicago"),
    ("calgary", "America/Edmonton"),
    ("beijing", "Asia/Shanghai"),
    ("shenzhen", "Asia/Shanghai"),
    ("guangzhou", "Asia/Shanghai"),
    ("delhi", "Asia/Kolkata"),
    ("new delhi", "Asia/Kolkata"),
    ("mumbai", "Asia/Kolkata"),
    ("bangalore", "Asia/Kolkata"),
    ("bengaluru", "Asia/Kolkata"),
    ("hyderabad", "Asia/Kolkata"),
    ("chennai", "Asia/Kolkata"),
    ("osaka", "Asia/Tokyo"),
    ("kyoto", "Asia/Tokyo"),
    ("tel aviv", "Asia/Jerusalem"),
    ("abu dhabi", "Asia/Dubai"),
    ("hanoi", "Asia/Ho_Chi_Minh"),
    ("milan", "Europe/Rome"),
    ("florence", "Europe/Rome"),
    ("barcelona", "Europe/Madrid"),
    ("munich", "Europe/Berlin"),
    ("frankfurt", "Europe/Berlin"),
    ("hamburg", "Europe/Berlin"),
    ("geneva", "Europe/Zurich"),
    ("edinburgh", "Europe/London"),
    ("manchester", "Europe/London"),
    ("rio", "America/Sao_Paulo"),
    ("rio de janeiro", "America/Sao_Paulo"),
    ("hawaii", "Pacific/Honolulu"),
    ("uk", "Europe/London"),
    ("japan", "Asia/Tokyo"),
    ("france", "Europe/Paris"),
    ("germany", "Europe/Berlin"),
    ("italy", "Europe/Rome"),
    ("spain", "Europe/Madrid"),
    ("india", "Asia/Kolkata"),
    ("china", "Asia/Shanghai"),
    ("korea", "Asia/Seoul"),
    ("south korea", "Asia/Seoul"),
];

fn place(text: &str, local: &str) -> Option<Place> {
    let key = text.trim().replace('_', " ");
    if key.is_empty() || key.len() > 40 {
        return None;
    }
    if matches!(
        key.as_str(),
        "here" | "local" | "me" | "my time" | "local time"
    ) {
        return Some(Place {
            zone: local.to_owned(),
            name: "you".to_owned(),
        });
    }
    if let Some((_, zone)) = ALIASES.iter().find(|(name, _)| *name == key) {
        return Some(Place {
            zone: (*zone).to_owned(),
            name: display(&key),
        });
    }
    cities()
        .iter()
        .find(|(city, _)| *city == key)
        .map(|(city, zone)| Place {
            zone: zone.clone(),
            name: display(city),
        })
}

fn display(key: &str) -> String {
    if key.len() <= 3 || matches!(key, "aest" | "aedt" | "cest") {
        return key.to_uppercase();
    }
    key.split(' ')
        .map(|word| {
            let mut letters = word.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every zone city under the region folders, as ("new york", "America/New_York"), read once.
fn cities() -> &'static [(String, String)] {
    static CITIES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CITIES.get_or_init(|| {
        let mut found = Vec::new();
        for region in [
            "Africa",
            "America",
            "Antarctica",
            "Arctic",
            "Asia",
            "Atlantic",
            "Australia",
            "Europe",
            "Indian",
            "Pacific",
        ] {
            collect(&Path::new(ZONEINFO).join(region), region, 0, &mut found);
        }
        found.sort();
        found
    })
}

fn collect(directory: &Path, id: &str, depth: usize, found: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{id}/{name}");
        let path = entry.path();
        if path.is_dir() && depth < 2 {
            collect(&path, &child, depth + 1, found);
        } else if path.is_file() {
            found.push((name.replace('_', " ").to_lowercase(), child));
        }
    }
}

// ---------------------------------------------------------------------------
// TZif
// ---------------------------------------------------------------------------

fn zone(id: &str) -> Option<Zone> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c));
    if !valid {
        return None;
    }
    let data = std::fs::read(Path::new(ZONEINFO).join(id)).ok()?;
    if data.len() > 1 << 20 {
        return None;
    }
    Zone::parse(&data)
}

#[derive(Debug)]
struct Zone {
    transitions: Vec<i64>,
    indices: Vec<u8>,
    /// (UTC offset in seconds, daylight saving, abbreviation).
    kinds: Vec<(i32, bool, String)>,
    rule: Option<Rule>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(slice)
    }

    fn header(&mut self) -> Option<(u8, [usize; 6])> {
        let header = self.take(44)?;
        if header.get(..4)? != b"TZif" {
            return None;
        }
        let mut counts = [0usize; 6];
        for (index, count) in counts.iter_mut().enumerate() {
            let bytes: [u8; 4] = header
                .get(20 + index * 4..24 + index * 4)?
                .try_into()
                .ok()?;
            *count = usize::try_from(u32::from_be_bytes(bytes)).ok()?;
        }
        Some((*header.get(4)?, counts))
    }
}

impl Zone {
    /// Version 2+ files repeat the data with 64-bit times after the 32-bit block, followed by a
    /// POSIX rule; the 64-bit data and the rule are what is read.
    fn parse(data: &[u8]) -> Option<Zone> {
        let mut reader = Reader { data, at: 0 };
        let (version, [utc_flags, standard_flags, leaps, times, kinds, characters]) =
            reader.header()?;
        let mut width = 4;
        let mut counts = (utc_flags, standard_flags, leaps, times, kinds, characters);
        if version >= b'2' {
            reader.take(
                times * 5 + kinds * 6 + characters + leaps * 8 + standard_flags + utc_flags,
            )?;
            let (_, [u, s, l, t, k, c]) = reader.header()?;
            counts = (u, s, l, t, k, c);
            width = 8;
        }
        let (utc_flags, standard_flags, leaps, times, kinds, characters) = counts;
        let transitions = reader
            .take(times.checked_mul(width)?)?
            .chunks_exact(width)
            .map(|bytes| match width {
                8 => bytes.try_into().map(i64::from_be_bytes).unwrap_or(0),
                _ => bytes
                    .try_into()
                    .map(|b| i64::from(i32::from_be_bytes(b)))
                    .unwrap_or(0),
            })
            .collect();
        let indices = reader.take(times)?.to_vec();
        let raw_kinds = reader.take(kinds.checked_mul(6)?)?;
        let names = reader.take(characters)?;
        reader.take(leaps * (width + 4) + standard_flags + utc_flags)?;
        let kinds: Vec<(i32, bool, String)> = raw_kinds
            .chunks_exact(6)
            .map(|kind| {
                let offset = i32::from_be_bytes([kind[0], kind[1], kind[2], kind[3]]);
                let name = names
                    .get(usize::from(kind[5])..)
                    .and_then(|rest| rest.split(|byte| *byte == 0).next())
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_default();
                (offset, kind[4] != 0, name)
            })
            .collect();
        if kinds.is_empty()
            || indices
                .iter()
                .any(|index| usize::from(*index) >= kinds.len())
        {
            return None;
        }
        let rule = (version >= b'2')
            .then(|| data.get(reader.at..))
            .flatten()
            .and_then(|footer| std::str::from_utf8(footer).ok())
            .and_then(|footer| footer.trim_start_matches('\n').split('\n').next())
            .and_then(Rule::parse);
        Some(Zone {
            transitions,
            indices,
            kinds,
            rule,
        })
    }

    /// The UTC offset and abbreviation in force at `instant`.
    fn at(&self, instant: i64) -> (i32, String) {
        let listed = self.transitions.partition_point(|&time| time <= instant);
        if listed == self.transitions.len()
            && let Some(rule) = &self.rule
        {
            return rule.at(instant);
        }
        let kind = if listed == 0 {
            self.kinds
                .iter()
                .find(|kind| !kind.1)
                .or(self.kinds.first())
        } else {
            self.indices
                .get(listed - 1)
                .and_then(|&index| self.kinds.get(usize::from(index)))
        };
        kind.map_or((0, "UTC".to_owned()), |kind| (kind.0, kind.2.clone()))
    }
}

/// A POSIX TZ rule such as `EST5EDT,M3.2.0,M11.1.0` or `<+09>-9`. POSIX offsets are written west
/// of UTC, so they are negated here.
#[derive(Debug, Clone)]
struct Rule {
    standard: (i32, String),
    daylight: Option<(i32, String, Transition, Transition)>,
}

#[derive(Debug, Clone, Copy)]
struct Transition {
    month: u32,
    week: u32,
    weekday: u32,
    /// Local wall-clock seconds after midnight; POSIX allows negative values and more than 24 h.
    seconds: i32,
}

impl Rule {
    fn parse(text: &str) -> Option<Rule> {
        let mut rest = text.trim();
        let standard_name = abbreviation(&mut rest)?;
        let standard = -signed_seconds(&mut rest)?;
        if rest.is_empty() {
            return Some(Rule {
                standard: (standard, standard_name),
                daylight: None,
            });
        }
        let daylight_name = abbreviation(&mut rest)?;
        let daylight = if rest.starts_with(',') {
            standard + 3600
        } else {
            -signed_seconds(&mut rest)?
        };
        let (start, end) = rest.strip_prefix(',')?.split_once(',')?;
        Some(Rule {
            standard: (standard, standard_name),
            daylight: Some((
                daylight,
                daylight_name,
                Transition::parse(start)?,
                Transition::parse(end)?,
            )),
        })
    }

    fn at(&self, instant: i64) -> (i32, String) {
        let Some((daylight, daylight_name, start, end)) = &self.daylight else {
            return self.standard.clone();
        };
        let year = civil_from_days((instant + i64::from(self.standard.0)).div_euclid(DAY)).0;
        let begins = start.local(year) - i64::from(self.standard.0);
        let ends = end.local(year) - i64::from(*daylight);
        // Southern-hemisphere rules start late in the year and end early in the next.
        let in_daylight = if begins < ends {
            instant >= begins && instant < ends
        } else {
            instant >= begins || instant < ends
        };
        if in_daylight {
            (*daylight, daylight_name.clone())
        } else {
            self.standard.clone()
        }
    }
}

impl Transition {
    /// `M3.2.0/2`: month 3, second week, Sunday, at 02:00. Week 5 means the last one.
    fn parse(text: &str) -> Option<Transition> {
        let (date, time) = match text.split_once('/') {
            Some((date, time)) => (date, Some(time)),
            None => (text, None),
        };
        let mut fields = date
            .strip_prefix('M')?
            .split('.')
            .map(|field| field.parse::<u32>().ok());
        let (month, week, weekday) = (fields.next()??, fields.next()??, fields.next()??);
        if !(1..=12).contains(&month) || !(1..=5).contains(&week) || weekday > 6 {
            return None;
        }
        let seconds = match time {
            Some(mut time) => signed_seconds(&mut time)?,
            None => 7200,
        };
        Some(Transition {
            month,
            week,
            weekday,
            seconds,
        })
    }

    /// Seconds since the epoch of this transition's local wall-clock moment in `year`.
    fn local(self, year: i64) -> i64 {
        let first = days_from_civil(year, self.month, 1);
        let first_weekday = (first + 4).rem_euclid(7);
        let mut day = 1
            + (i64::from(self.weekday) - first_weekday).rem_euclid(7)
            + (i64::from(self.week) - 1) * 7;
        while day > days_in_month(year, self.month) {
            day -= 7;
        }
        (first + day - 1) * DAY + i64::from(self.seconds)
    }
}

fn abbreviation(rest: &mut &str) -> Option<String> {
    if let Some(quoted) = rest.strip_prefix('<') {
        let (name, after) = quoted.split_once('>')?;
        *rest = after;
        return Some(name.to_owned());
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    if end < 3 {
        return None;
    }
    let (name, after) = rest.split_at(end);
    *rest = after;
    Some(name.to_owned())
}

fn signed_seconds(rest: &mut &str) -> Option<i32> {
    let negative = rest.starts_with('-');
    if let Some(unsigned) = rest.strip_prefix(['+', '-']) {
        *rest = unsigned;
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(rest.len());
    let (text, after) = rest.split_at(end);
    *rest = after;
    if text.is_empty() {
        return None;
    }
    let mut parts = text.split(':').map(|part| part.parse::<i32>().ok());
    let hours = parts.next()??;
    let minutes = parts.next().unwrap_or(Some(0))?;
    let seconds = parts.next().unwrap_or(Some(0))?;
    if hours > 167 || minutes > 59 || seconds > 59 {
        return None;
    }
    let total = hours * 3600 + minutes * 60 + seconds;
    Some(if negative { -total } else { total })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * shifted + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    })
    .unwrap_or(1);
    let year = year_of_era + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn days_in_month(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i64, month: u32, day: u32, hour: i64) -> i64 {
        days_from_civil(year, month, day) * DAY + hour * 3600
    }

    fn values(query: &str, now: i64) -> Vec<String> {
        rows(query, now, "America/Toronto")
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.value)
            .collect()
    }

    #[test]
    fn conversions_follow_daylight_saving_in_both_places() {
        assert_eq!(
            values("3pm montreal in tokyo", at(2026, 9, 15, 15)),
            [
                "4:00 AM · Wed",
                "3:00 PM · Tue",
                "Tokyo is 13 h ahead of Montreal"
            ]
        );
        assert_eq!(
            values("3pm Montreal to Tokyo", at(2026, 1, 15, 12)),
            [
                "5:00 AM · Fri",
                "3:00 PM · Thu",
                "Tokyo is 14 h ahead of Montreal"
            ]
        );
        assert_eq!(
            values("15:00 utc in montreal", at(2026, 9, 15, 12)),
            [
                "11:00 AM · Tue",
                "3:00 PM · Tue",
                "Montreal is 4 h behind UTC"
            ]
        );
        assert_eq!(
            values("9am to london", at(2026, 9, 15, 12)),
            [
                "2:00 PM · Tue",
                "9:00 AM · Tue",
                "London is 5 h ahead of you"
            ]
        );
        assert_eq!(
            values("noon in tokyo", at(2026, 9, 15, 12)),
            [
                "11:00 PM · Mon",
                "12:00 PM · Tue",
                "You are 13 h behind Tokyo"
            ]
        );
        assert_eq!(
            values("5:30 pm delhi to montreal", at(2026, 9, 15, 12))[2],
            "Montreal is 9 h 30 min behind Delhi"
        );
    }

    #[test]
    fn current_time_anywhere() {
        assert_eq!(
            values("time in paris", at(2026, 9, 15, 15)),
            ["5:00 PM · Tue", "CEST · UTC+2", "Paris is 6 h ahead of you"]
        );
        assert_eq!(
            values("Sydney time", at(2026, 1, 15, 12)),
            [
                "11:00 PM · Thu",
                "AEDT · UTC+11",
                "Sydney is 16 h ahead of you"
            ]
        );
        let later = values("what time is it in new york?", at(2045, 7, 1, 12));
        assert!(later[0].starts_with("8:00 AM"), "{later:?}");
        assert_eq!(later[1..], ["EDT · UTC-4", "Same time as you"]);
    }

    #[test]
    fn footer_rules_cover_both_hemispheres() {
        let north = Rule::parse("EST5EDT,M3.2.0,M11.1.0").expect("rule");
        assert_eq!(north.at(at(2050, 7, 1, 12)), (-4 * 3600, "EDT".to_owned()));
        assert_eq!(north.at(at(2050, 12, 1, 12)).0, -5 * 3600);
        let south = Rule::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").expect("rule");
        assert_eq!(south.at(at(2050, 1, 15, 0)).0, 11 * 3600);
        assert_eq!(south.at(at(2050, 6, 15, 0)).0, 10 * 3600);
        assert_eq!(
            Rule::parse("<+09>-9").expect("rule").at(0),
            (9 * 3600, "+09".to_owned())
        );
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn unknown_places_and_bare_numbers_produce_nothing() {
        for query in [
            "3pm atlantis in tokyo",
            "4 in a row",
            "time in",
            "15 apples in paris",
            "work time",
            "3pm",
            "time in paris france",
            "25:00 in paris",
        ] {
            assert!(
                rows(query, at(2026, 9, 15, 12), "America/Toronto").is_none(),
                "{query}"
            );
        }
    }
}
