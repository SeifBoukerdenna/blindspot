//! Splitting a file's text into the passages search returns.
//!
//! One vector per file can only ever match a file's opening; a passage is what a person actually
//! wants back: the page of a PDF, the function in a source file, the section of a note. Chunk
//! boundaries therefore follow the shape of the text rather than a byte count alone: paragraphs
//! and headings in prose, declarations in code, page breaks in extracted documents.

/// How a file's text is shaped, which decides where a chunk may start and how it is located.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Notes, Markdown, HTML: paragraphs under headings.
    Prose,
    /// Source code: declarations and identifiers.
    Code,
    /// Extracted documents, whose pages the helper separates with form feeds.
    Paged,
    Sections,
}

/// Where a chunk sits in its file, which is what "open at" uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Location {
    Line(u32),
    Page(u32),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chunk {
    /// Position in the file, counted across pages for a paged document.
    pub ordinal: u32,
    pub location: Location,
    /// The headings above this passage ("Renewal proposal > What changes"), or the declaration it
    /// sits in for code. Searched and shown, so a passage says where it came from.
    pub heading: String,
    pub text: String,
    /// Identifiers split into words, so `searchFiltered` is also found as "search filtered".
    pub symbols: String,
}

/// The shape of the passages, and the ceilings a single file may not pass.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub target_bytes: usize,
    /// Text repeated from the end of the previous passage, so a sentence split across a boundary
    /// is still whole in one of them.
    pub overlap_bytes: usize,
    pub max_bytes: usize,
    pub max_chunks: usize,
}

impl Default for Limits {
    /// About a paragraph per passage, with a sentence of overlap. The ceilings are per file and
    /// deliberately both: a 50 MB log is stopped by bytes, a 5,000-page PDF by chunks.
    fn default() -> Self {
        Self {
            target_bytes: 1_000,
            overlap_bytes: 150,
            max_bytes: 2 * 1024 * 1024,
            max_chunks: 400,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Chunked {
    pub chunks: Vec<Chunk>,
    /// The file was longer than a ceiling, so what is indexed is a prefix of it.
    pub truncated: bool,
}

pub fn chunks(kind: Kind, text: &str, limits: &Limits) -> Chunked {
    let mut truncated = false;
    let text = if text.len() > limits.max_bytes {
        truncated = true;
        &text[..text.floor_char_boundary(limits.max_bytes)]
    } else {
        text
    };
    let mut out = Vec::new();
    let complete = match kind {
        Kind::Prose => pack(&blocks(text), limits, None, &mut out),
        Kind::Code => code(text, limits, &mut out),
        Kind::Paged => text
            .split('\u{c}')
            .enumerate()
            .all(|(index, page)| {
                let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
                pack(&blocks(page), limits, Some(number), &mut out)
            }),
        Kind::Sections => text.split('\u{c}').all(|section|pack(&blocks(section),limits,None,&mut out)),
    };
    Chunked { chunks: out, truncated: truncated || !complete }
}

// ---------------------------------------------------------------------------
// Prose
// ---------------------------------------------------------------------------

/// A paragraph, with the line it starts on and the headings above it.
struct Block {
    line: u32,
    heading: String,
    text: String,
}

fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut crumbs: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    let mut start = 1;
    for (index, line) in text.lines().enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let trimmed = line.trim();
        if let Some(level) = heading_level(trimmed) {
            close(&mut blocks, &mut current, start, &crumbs);
            crumbs.retain(|(depth, _)| *depth < level);
            crumbs.push((level, trimmed.trim_start_matches('#').trim().to_owned()));
            start = number + 1;
        } else if trimmed.is_empty() {
            close(&mut blocks, &mut current, start, &crumbs);
            start = number + 1;
        } else {
            if current.is_empty() {
                start = number;
            }
            current.push_str(line.trim_end());
            current.push('\n');
        }
    }
    close(&mut blocks, &mut current, start, &crumbs);
    blocks
}

fn close(blocks: &mut Vec<Block>, current: &mut String, line: u32, crumbs: &[(usize, String)]) {
    if current.trim().is_empty() {
        current.clear();
        return;
    }
    blocks.push(Block {
        line,
        heading: crumbs.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>().join(" > "),
        text: std::mem::take(current),
    });
}

/// `#`, `##` … as Markdown writes them; a line of hashes alone is not a heading.
fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&hashes).then_some(hashes).filter(|_| !line[hashes..].trim().is_empty())
}

/// Fills passages with whole paragraphs, carrying a little of the previous passage into the next.
/// Returns false once the chunk ceiling is reached.
fn pack(blocks: &[Block], limits: &Limits, page: Option<u32>, out: &mut Vec<Chunk>) -> bool {
    let mut text = String::new();
    let mut heading = String::new();
    let mut line = 1;
    for block in blocks {
        if !text.is_empty() && text.len() + block.text.len() > limits.target_bytes {
            if !emit(out, limits, page, line, &heading, &text) {
                return false;
            }
            text = overlap(&text, limits.overlap_bytes);
            line = block.line;
        }
        if text.trim().is_empty() {
            line = block.line;
            heading.clone_from(&block.heading);
        }
        text.push_str(&block.text);
        // One enormous paragraph (a minified file, a wall of extracted text) still has to be cut.
        while text.len() > limits.target_bytes * 2 {
            let cut = text.floor_char_boundary(limits.target_bytes);
            let rest = text.split_off(cut);
            if !emit(out, limits, page, line, &heading, &text) {
                return false;
            }
            text = rest;
        }
    }
    emit(out, limits, page, line, &heading, &text)
}

/// The tail of a passage, started at a sentence or word boundary so the repeated text reads.
fn overlap(text: &str, bytes: usize) -> String {
    if bytes == 0 || text.len() <= bytes {
        return String::new();
    }
    let tail = &text[text.ceil_char_boundary(text.len() - bytes)..];
    let start = tail
        .find(". ")
        .map(|at| at + 2)
        .or_else(|| tail.find(' ').map(|at| at + 1))
        .unwrap_or(0);
    let mut carried = tail[start..].to_owned();
    if !carried.is_empty() && !carried.ends_with('\n') {
        carried.push('\n');
    }
    carried
}

// ---------------------------------------------------------------------------
// Code
// ---------------------------------------------------------------------------

/// Words that start a declaration in the languages this indexes. Modifiers are skipped first, so
/// `pub async fn` and `export default class` are recognised by their keyword.
const DECLARES: &[&str] = &[
    "fn", "func", "function", "def", "class", "struct", "enum", "impl", "trait", "interface",
    "module", "package", "protocol", "extension", "object", "record", "namespace",
    "macro_rules!", "sub", "proc",
];
/// Bindings only start a passage at column zero: indented, they are a function's own locals, and
/// breaking there cuts the function in half.
const BINDINGS: &[&str] = &["const", "let", "var", "val", "static", "type"];
const MODIFIERS: &[&str] = &[
    "pub", "pub(crate)", "public", "private", "protected", "internal", "export", "default",
    "async", "static", "final", "override", "open", "abstract", "unsafe", "extern", "inline",
    "declare", "@objc", "@main", "#[derive]",
];

/// Breaks at declarations once a passage is worth keeping, so a function lands whole where it can.
fn code(text: &str, limits: &Limits, out: &mut Vec<Chunk>) -> bool {
    let mut current = String::new();
    let mut lines = 0;
    let mut start = 1;
    let mut heading = String::new();
    let mut next_heading = String::new();
    for (index, line) in text.lines().enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let declaration = declares(line);
        let worth_keeping = current.len() >= limits.target_bytes / 2 || lines >= 20;
        if declaration && worth_keeping {
            if !emit(out, limits, None, start, &heading, &current) {
                return false;
            }
            current.clear();
            lines = 0;
        }
        if current.trim().is_empty() {
            start = number;
            heading = if declaration { headline(line) } else { std::mem::take(&mut next_heading) };
        }
        if declaration && heading.is_empty() {
            heading = headline(line);
        }
        if declaration {
            next_heading = headline(line);
        }
        current.push_str(line);
        current.push('\n');
        lines += 1;
        // A file with no declarations at all (data, a template) is cut by size instead.
        if current.len() > limits.target_bytes * 2 {
            if !emit(out, limits, None, start, &heading, &current) {
                return false;
            }
            current.clear();
            lines = 0;
        }
    }
    emit(out, limits, None, start, &heading, &current)
}

fn declares(line: &str) -> bool {
    // Column zero or one level in: a nested closure is not where a passage should start.
    let indent = line.len() - line.trim_start().len();
    if indent > 4 {
        return false;
    }
    let mut words = line.split_whitespace().skip_while(|word| {
        MODIFIERS.contains(&word.trim_end_matches(':').to_ascii_lowercase().as_str())
    });
    let Some(word) = words.next() else { return false };
    let word = word.trim_end_matches(['(', ':']).to_ascii_lowercase();
    if BINDINGS.contains(&word.as_str()) {
        return indent == 0;
    }
    DECLARES.contains(&word.as_str())
}

/// The declaration line itself, tidied into a heading.
fn headline(line: &str) -> String {
    let trimmed = line.trim().trim_end_matches(['{', ':', ';']).trim();
    trimmed[..trimmed.floor_char_boundary(100)].to_owned()
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

fn emit(
    out: &mut Vec<Chunk>,
    limits: &Limits,
    page: Option<u32>,
    line: u32,
    heading: &str,
    text: &str,
) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    if out.len() >= limits.max_chunks {
        return false;
    }
    out.push(Chunk {
        ordinal: u32::try_from(out.len()).unwrap_or(u32::MAX),
        location: page.map_or(Location::Line(line.max(1)), Location::Page),
        heading: heading.to_owned(),
        text: trimmed.to_owned(),
        symbols: symbols(trimmed),
    });
    true
}

/// `search_filtered`, `searchFiltered` and `SearchFiltered` all become "search filtered", so a
/// passage is found however the name is written. Capped, since this is a search aid, not a copy.
fn symbols(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for identifier in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if identifier.len() < 3 || identifier.len() > 64 || !identifier.contains(['_'] ) && !identifier.chars().any(char::is_uppercase) {
            continue;
        }
        for part in split_identifier(identifier) {
            if part.len() >= 2 && seen.insert(part.to_ascii_lowercase()) {
                words.push(part.to_ascii_lowercase());
            }
        }
    }
    let mut joined = String::new();
    for word in words {
        if joined.len() + word.len() + 1 > 2_000 {
            break;
        }
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(&word);
    }
    joined
}

fn split_identifier(identifier: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for piece in identifier.split('_').filter(|piece| !piece.is_empty()) {
        let mut word = String::new();
        for character in piece.chars() {
            if character.is_uppercase() && !word.is_empty() && !word.ends_with(char::is_uppercase) {
                parts.push(std::mem::take(&mut word));
            }
            word.push(character);
        }
        if !word.is_empty() {
            parts.push(word);
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(chunked: &Chunked) -> Vec<&str> {
        chunked.chunks.iter().map(|chunk| chunk.text.as_str()).collect()
    }

    #[test]
    fn prose_breaks_at_paragraphs_and_carries_its_headings() {
        let note = "# Renewal\n\nNorthwind renews on October 31.\n\n## What changes\n\nPricing moves \
                    to per-site licensing.\n\nSupport improves to four hours.\n";
        let chunked = chunks(Kind::Prose, note, &Limits { target_bytes: 60, ..Limits::default() });
        assert!(chunked.chunks.len() >= 3, "{:?}", texts(&chunked));
        assert_eq!(chunked.chunks[0].heading, "Renewal");
        assert_eq!(chunked.chunks[0].location, Location::Line(3));
        assert!(chunked.chunks[1].heading.starts_with("Renewal > What changes"));
        assert!(!chunked.truncated);
        assert!(chunked.chunks.iter().all(|chunk| !chunk.text.contains("##")));
    }

    #[test]
    fn overlap_repeats_the_end_of_the_previous_passage() {
        let note = "First sentence here. Second sentence here.\n\nA new paragraph that is long \
                    enough to force a break.\n";
        let chunked = chunks(Kind::Prose, note, &Limits { target_bytes: 50, overlap_bytes: 25, ..Limits::default() });
        assert!(chunked.chunks.len() >= 2);
        assert!(chunked.chunks[1].text.contains("Second sentence here."), "{:?}", texts(&chunked));
    }

    #[test]
    fn code_starts_passages_at_declarations() {
        let source = "use std::fs;\n\npub fn search_filtered(query: &str) -> usize {\n".to_owned()
            + &"    let _ = query;\n".repeat(25)
            + "}\n\nfn rebuild_shard() {\n"
            + &"    let _ = 1;\n".repeat(25)
            + "}\n";
        let chunked = chunks(Kind::Code, &source, &Limits::default());
        assert_eq!(chunked.chunks.len(), 2, "{:?}", texts(&chunked));
        assert!(chunked.chunks[0].heading.contains("search_filtered"));
        assert!(chunked.chunks[1].heading.contains("rebuild_shard"));
        assert_eq!(chunked.chunks[1].location, Location::Line(31));
        assert!(chunked.chunks[0].symbols.contains("search filtered"), "{}", chunked.chunks[0].symbols);
    }

    #[test]
    fn pages_keep_their_numbers() {
        let document = "Cover page\u{c}Renewal terms and the deadline\u{c}Signatures";
        let chunked = chunks(Kind::Paged, document, &Limits::default());
        assert_eq!(chunked.chunks.len(), 3);
        assert_eq!(chunked.chunks[1].location, Location::Page(2));
        assert_eq!(chunked.chunks[2].ordinal, 2);
        let sheets=chunks(Kind::Sections,"# Sheet One\n\nRow 1: value\u{c}# Sheet Two\n\nRow 8: other",&Limits::default());
        assert_eq!(sheets.chunks.len(),2);
        assert_eq!(sheets.chunks[1].heading,"Sheet Two");
    }

    #[test]
    fn both_ceilings_stop_a_huge_file() {
        let many = "word ".repeat(20_000);
        let by_chunks = chunks(Kind::Prose, &many, &Limits { target_bytes: 100, max_chunks: 5, ..Limits::default() });
        assert!(by_chunks.truncated && by_chunks.chunks.len() == 5);
        let by_bytes = chunks(Kind::Prose, &many, &Limits { max_bytes: 500, ..Limits::default() });
        assert!(by_bytes.truncated);
        assert!(by_bytes.chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>() <= 500);
    }

    #[test]
    fn identifiers_are_split_for_search() {
        assert_eq!(split_identifier("searchFiltered"), ["search", "Filtered"]);
        assert_eq!(split_identifier("content_fts"), ["content", "fts"]);
        assert_eq!(split_identifier("HTTPServer"), ["HTTPServer"]);
        let chunk = &chunks(Kind::Code, "let vectorShard = rebuild_cache();\n", &Limits::default()).chunks[0];
        assert!(chunk.symbols.contains("vector shard") && chunk.symbols.contains("rebuild cache"), "{}", chunk.symbols);
    }

    #[test]
    fn nothing_from_nothing() {
        for text in ["", "   \n\n\t\n"] {
            assert!(chunks(Kind::Prose, text, &Limits::default()).chunks.is_empty());
            assert!(chunks(Kind::Code, text, &Limits::default()).chunks.is_empty());
        }
    }
}
