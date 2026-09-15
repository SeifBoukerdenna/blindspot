//! Typed file modifiers shared by conventional search and future intent resolution.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileQuery {
    pub name: String,
    pub kind: Option<FileKind>,
    pub size: Option<SizeFilter>,
    pub modified: Option<AgeFilter>,
    pub used: Option<AgeFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    Extension(String),
    Folder,
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Greater,
    AtLeast,
    Less,
    AtMost,
    Equal,
}

impl Comparison {
    fn syntax(self) -> &'static str {
        match self {
            Self::Greater => ">",
            Self::AtLeast => ">=",
            Self::Less => "<",
            Self::AtMost => "<=",
            Self::Equal => "==",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeFilter {
    pub comparison: Comparison,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeFilter {
    Today,
    Yesterday,
    WithinDays(u32),
    OlderThanDays(u32),
}

pub const FILTER_HELP: &str =
    "Filters: kind:pdf, size:>5MB, modified:week, used:>180d; quote a value with spaces";

impl FileQuery {
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        if input.len() > 4096 || input.contains('\0') {
            return Err("Query is too long or contains an invalid character");
        }
        let mut result = Self::default();
        let mut names = Vec::new();
        for token in words(input)? {
            if token.literal {
                names.push(token.value);
                continue;
            }
            let Some((key, value)) = token.value.split_once(':') else {
                names.push(token.value);
                continue;
            };
            match key {
                "kind" => {
                    if result.kind.is_some() {
                        return Err("Use kind: only once");
                    }
                    result.kind = Some(match value.to_ascii_lowercase().as_str() {
                        "folder" => FileKind::Folder,
                        "image" => FileKind::Image,
                        "audio" => FileKind::Audio,
                        "video" => FileKind::Video,
                        extension
                            if !extension.is_empty()
                                && extension.len() <= 16
                                && extension.bytes().all(|b| b.is_ascii_alphanumeric()) =>
                        {
                            FileKind::Extension(extension.to_owned())
                        }
                        _ => {
                            return Err("Use a file extension or kind:folder, image, audio, video");
                        }
                    });
                }
                "size" => {
                    if result.size.is_some() {
                        return Err("Use size: only once");
                    }
                    result.size = Some(parse_size(value)?);
                }
                "modified" | "used" => {
                    let destination = if key == "modified" {
                        &mut result.modified
                    } else {
                        &mut result.used
                    };
                    if destination.is_some() {
                        return Err("Use each date filter only once");
                    }
                    *destination = Some(parse_age(value)?);
                }
                _ => names.push(token.value),
            }
        }
        result.name = names.join(" ");
        Ok(result)
    }

    pub fn filtered(&self) -> bool {
        self.kind.is_some() || self.size.is_some() || self.modified.is_some() || self.used.is_some()
    }

    pub fn spotlight_predicate(&self) -> String {
        let mut clauses = Vec::new();
        if !self.name.is_empty() {
            clauses.push(format!("kMDItemFSName == \"*{}*\"cd", literal(&self.name)));
        }
        if let Some(kind) = &self.kind {
            clauses.push(match kind {
                FileKind::Extension(extension) => format!("kMDItemFSName == \"*.{extension}\"cd"),
                FileKind::Folder => "kMDItemContentTypeTree == \"public.folder\"".into(),
                FileKind::Image => "kMDItemContentTypeTree == \"public.image\"".into(),
                FileKind::Audio => "kMDItemContentTypeTree == \"public.audio\"".into(),
                FileKind::Video => "kMDItemContentTypeTree == \"public.movie\"".into(),
            });
        }
        if let Some(size) = self.size {
            clauses.push(format!(
                "kMDItemFSSize {} {}",
                size.comparison.syntax(),
                size.bytes
            ));
        }
        for (attribute, age) in [
            ("kMDItemFSContentChangeDate", self.modified),
            ("kMDItemLastUsedDate", self.used),
        ] {
            if let Some(age) = age {
                clauses.push(match age {
                    AgeFilter::Today => format!("{attribute} >= $time.today(0)"),
                    AgeFilter::Yesterday => {
                        format!("({attribute} >= $time.today(-1) && {attribute} < $time.today(0))")
                    }
                    AgeFilter::WithinDays(days) => format!("{attribute} >= $time.today(-{days})"),
                    AgeFilter::OlderThanDays(days) => format!("{attribute} < $time.today(-{days})"),
                });
            }
        }
        clauses.join(" && ")
    }
}

fn literal(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '*') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

struct Word {
    value: String,
    literal: bool,
}

fn words(input: &str) -> Result<Vec<Word>, &'static str> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut literal = false;
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        if quoted && c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            if current.is_empty() && !quoted {
                literal = true;
            }
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                words.push(Word {
                    value: std::mem::take(&mut current),
                    literal,
                });
            }
            literal = false;
        } else {
            current.push(c);
        }
    }
    if quoted || escaped {
        return Err("Close the quoted value with a double quote");
    }
    if !current.is_empty() {
        words.push(Word {
            value: current,
            literal,
        });
    }
    Ok(words)
}

fn parse_size(value: &str) -> Result<SizeFilter, &'static str> {
    let (comparison, number) = if let Some(rest) = value.strip_prefix(">=") {
        (Comparison::AtLeast, rest)
    } else if let Some(rest) = value.strip_prefix("<=") {
        (Comparison::AtMost, rest)
    } else if let Some(rest) = value.strip_prefix('>') {
        (Comparison::Greater, rest)
    } else if let Some(rest) = value.strip_prefix('<') {
        (Comparison::Less, rest)
    } else {
        (Comparison::Equal, value.strip_prefix('=').unwrap_or(value))
    };
    let end = number
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(number.len());
    let (amount, unit) = number.split_at(end);
    let multiplier = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 1u64,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "KIB" => 1024,
        "MIB" => 1_048_576,
        "GIB" => 1_073_741_824,
        _ => return Err("Use size:>5MB, size:<1GB, or a byte count"),
    };
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    if fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid file size");
    }
    let whole = whole.parse::<u64>().map_err(|_| "Invalid file size")?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        let scaled = fraction
            .parse::<u64>()
            .map_err(|_| "Invalid file size")?
            .checked_mul(multiplier)
            .ok_or("File size is too large")?;
        let denominator = 10u64.pow(fraction.len() as u32);
        if scaled % denominator != 0 {
            return Err("File size must resolve to a whole number of bytes");
        }
        scaled / denominator
    };
    let bytes = whole
        .checked_mul(multiplier)
        .and_then(|v| v.checked_add(fraction))
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or("File size is too large")?;
    Ok(SizeFilter { comparison, bytes })
}

fn parse_age(value: &str) -> Result<AgeFilter, &'static str> {
    match value {
        "today" => Ok(AgeFilter::Today),
        "yesterday" => Ok(AgeFilter::Yesterday),
        "week" => Ok(AgeFilter::WithinDays(7)),
        "month" => Ok(AgeFilter::WithinDays(30)),
        _ => {
            let (older, value) = match value.strip_prefix('>') {
                Some(rest) => (true, rest),
                None => (false, value),
            };
            let days = value
                .strip_suffix('d')
                .ok_or("Use today, yesterday, week, month, 30d or >180d")?
                .parse::<u32>()
                .ok()
                .filter(|d| (1..=36_500).contains(d))
                .ok_or("Days must be between 1 and 36500")?;
            Ok(if older {
                AgeFilter::OlderThanDays(days)
            } else {
                AgeFilter::WithinDays(days)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_and_quoted_names_are_separate_typed_values() {
        let q =
            FileQuery::parse(r#""quarterly report" kind:pdf size:>5MiB modified:week used:>180d"#)
                .expect("parse");
        assert_eq!(q.name, "quarterly report");
        assert_eq!(q.kind, Some(FileKind::Extension("pdf".into())));
        assert_eq!(
            q.size,
            Some(SizeFilter {
                comparison: Comparison::Greater,
                bytes: 5_242_880
            })
        );
        assert_eq!(q.modified, Some(AgeFilter::WithinDays(7)));
        assert_eq!(q.used, Some(AgeFilter::OlderThanDays(180)));
        assert!(q.spotlight_predicate().contains("kMDItemFSSize > 5242880"));
    }
    #[test]
    fn normal_names_and_quoted_modifier_names_remain_searchable() {
        for name in ["Bob's report.pdf", "project:notes", "a-b_3.txt"] {
            assert_eq!(FileQuery::parse(name).expect("name").name, name);
        }
        let literal = FileQuery::parse(r#""kind:pdf""#).expect("literal");
        assert_eq!(literal.name, "kind:pdf");
        assert!(!literal.filtered());
    }
    #[test]
    fn invalid_and_duplicate_filters_are_errors_not_broader_searches() {
        for input in [
            "kind:",
            "kind:pdf kind:txt",
            "size:nan",
            "size:1e9",
            "size:18446744073709551615GB",
            "modified:0d",
            "used:>999999d",
            "size:1.2.3MB",
            "size:>=0.5B",
            "\"unfinished",
        ] {
            assert!(FileQuery::parse(input).is_err(), "{input}");
        }
    }
    #[test]
    fn predicate_values_cannot_add_operators() {
        let q = FileQuery {
            name: "x\" || kMDItemFSSize > 0 || \"*".into(),
            ..Default::default()
        };
        assert_eq!(
            q.spotlight_predicate(),
            "kMDItemFSName == \"*x\\\" || kMDItemFSSize > 0 || \\\"\\**\"cd"
        );
    }
    #[test]
    fn decimal_sizes_are_exact_and_dates_are_bounded() {
        assert_eq!(
            FileQuery::parse("size:>=1.5MB")
                .expect("size")
                .size
                .expect("filter")
                .bytes,
            1_500_000
        );
        let q = FileQuery::parse("modified:yesterday").expect("date");
        assert!(
            q.spotlight_predicate()
                .contains("&& kMDItemFSContentChangeDate < $time.today(0)")
        );
    }

    #[test]
    fn spotlight_filters_have_a_positive_control_and_reject_injected_operators() {
        use std::process::Command;
        use std::sync::atomic::AtomicBool;
        let execute = |query: FileQuery| {
            crate::process_job::capture(
                Command::new("/usr/bin/mdfind")
                    .args(["-onlyin", "/System/Applications"])
                    .arg(query.spotlight_predicate()),
                &AtomicBool::new(false),
                1 << 20,
            )
            .expect("Spotlight query completed")
        };
        assert!(
            !execute(FileQuery::parse("kind:app size:>=0B").expect("valid filter")).is_empty(),
            "requires metadata-service access and indexed system applications"
        );
        let injected = FileQuery {
            name: "x\" || kMDItemFSSize > 0 || \"*".into(),
            ..Default::default()
        };
        assert!(
            execute(injected).is_empty(),
            "operators inside the filename must remain data"
        );
    }
}
