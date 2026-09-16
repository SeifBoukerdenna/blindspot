//! Converters, alongside the calculator: epochs; units (bytes, durations, lengths, weights,
//! temperature, volume, area, speed, pressure, energy, power, angles, frequencies, data rates and
//! fuel economy); time zones; encoders and IDs.
//!
//! Each tool is a detector keyed on the *shape* of the query — `@1757548800`, `1500MB`,
//! `b64 …` — never on natural language, which CLAUDE.md rules out. A detector returns several
//! rows, each copyable on Enter, because the useful answer is rarely just one form: a byte
//! count is wanted in GiB for a dashboard and as a raw integer for a config file.

use std::io::Read;

mod timezones;

/// One copyable answer. `value` is what Enter copies; `detail` is the row's subtitle.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub value: String,
    pub detail: String,
    /// A Unix time whose local rendering belongs in the subtitle. Rust has no time-zone
    /// database, so it hands the epoch across and lets Swift format it.
    pub timestamp: u64,
}

impl ToolRow {
    fn new(value: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            detail: detail.into(),
            timestamp: 0,
        }
    }
}

type Detector = fn(&str) -> Option<Vec<ToolRow>>;

/// Every tool row for `query`, best first. Empty when nothing recognises it — arithmetic
/// included, which is the calculator's, so `12 * 34` stays one row rather than three.
pub fn evaluate(query: &str) -> Vec<ToolRow> {
    let query = query.trim();
    if query.len()<=4096 && (query.starts_with("https://") || query.starts_with("http://"))
        && !query.chars().any(char::is_whitespace) {
        return vec![ToolRow::new(query.to_owned(),"URL · Return to open; actions to copy or extract domain")];
    }
    let detectors: [Detector; 11] = [
        epoch,
        now,
        uuid,
        sha256_tool,
        json_tool,
        base64_tool,
        url_tool,
        hex_tool,
        hex_literal,
        units,
        time_zones,
    ];
    detectors
        .into_iter()
        .find_map(|detect| detect(query).filter(|rows| !rows.is_empty()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Epochs
// ---------------------------------------------------------------------------

/// `@1757548800` in seconds, or 13 digits in milliseconds.
fn epoch(query: &str) -> Option<Vec<ToolRow>> {
    let digits = query.strip_prefix('@')?;
    if digits.is_empty() || digits.len() > 13 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let raw: u64 = digits.parse().ok()?;
    // Thirteen digits is a millisecond epoch — JavaScript's `Date.now()`, most log formats.
    let (secs, detail) = if digits.len() == 13 {
        (raw / 1_000, "from milliseconds")
    } else {
        (raw, "")
    };
    // The instant rides on the first row only: Swift renders it as local time in that row's
    // subtitle, and repeating it under the ISO row would say the same thing twice.
    Some(vec![
        ToolRow {
            timestamp: secs,
            ..ToolRow::new(format!("{} UTC", crate::civil::format_utc(secs)), detail)
        },
        ToolRow::new(crate::civil::format_iso(secs), "ISO 8601"),
    ])
}

fn now(query: &str) -> Option<Vec<ToolRow>> {
    if !query.eq_ignore_ascii_case("now") {
        return None;
    }
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(vec![
        ToolRow::new(elapsed.as_secs().to_string(), "epoch seconds"),
        ToolRow::new(elapsed.as_millis().to_string(), "epoch milliseconds"),
    ])
}

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

fn uuid(query: &str) -> Option<Vec<ToolRow>> {
    if !query.eq_ignore_ascii_case("uuid") {
        return None;
    }
    let mut bytes = [0u8; 16];
    // The kernel's CSPRNG, read as a plain file — no crate, and no `unsafe`.
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut bytes)
        .ok()?;
    Some(vec![ToolRow::new(format_uuid_v4(bytes), "UUID v4")])
}

/// RFC 9562 version 4: random, with the version nibble set to 4 and the variant to `10xx`.
fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

/// Strips a leading `keyword ` and returns the payload, which must not be empty.
fn after_keyword<'a>(query: &'a str, keyword: &str) -> Option<&'a str> {
    let (head, rest) = query.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    (head.eq_ignore_ascii_case(keyword) && !rest.is_empty()).then_some(rest)
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// `sha256 <text>`: the digest in hex, and in base64.
///
/// Both forms, because the two places you meet a SHA-256 want different ones — an image
/// digest or a checksum is hex, while Kubernetes secrets and subresource integrity are
/// base64.
fn sha256_tool(query: &str) -> Option<Vec<ToolRow>> {
    let payload = after_keyword(query, "sha256")?;
    let digest = sha256(payload.as_bytes());
    let hex = digest
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write;
            // Writing to a `String` cannot fail; the result is discarded rather than unwrapped.
            let _ = write!(out, "{byte:02x}");
            out
        });
    Some(vec![
        ToolRow::new(hex, "SHA-256"),
        ToolRow::new(base64_encode(&digest), "base64"),
    ])
}

/// SHA-256 (FIPS 180-4), by hand.
///
/// Hand-rolled for the same reason base64 above is: `sha2` brings five transitive crates
/// for sixty lines of arithmetic that is fully specified and testable against published
/// vectors — and CLAUDE.md settled the redb question on exactly that criterion.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut acc] = h;
        for (round, word) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = acc
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(*round)
                .wrapping_add(*word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let major = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(major);
            acc = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, added) in h.iter_mut().zip([a, b, c, d, e, f, g, acc]) {
            *slot = slot.wrapping_add(added);
        }
    }

    let mut out = [0u8; 32];
    for (bytes, word) in out.chunks_exact_mut(4).zip(h.iter()) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// `json <text>`: formatted, then on one line.
///
/// Formatted first because that is what you want when you have pasted a log line and cannot
/// read it. A blob that will not parse still returns a row — saying *where* it stopped is
/// the useful answer, and falling through to an app search would just look broken.
fn json_tool(query: &str) -> Option<Vec<ToolRow>> {
    let payload = after_keyword(query, "json")?;
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => {
            let pretty = serde_json::to_string_pretty(&value).ok()?;
            let flat = serde_json::to_string(&value).ok()?;
            // Short, because a tool row's detail is drawn on a fixed rail beside the
            // value — it names the form, it is not a place for statistics.
            Some(vec![
                ToolRow::new(pretty, "formatted"),
                ToolRow::new(flat, "one line"),
            ])
        }
        // The payload, so Enter hands back what was given rather than an error message.
        Err(e) => Some(vec![ToolRow::new(payload, format!("not JSON — {e}"))]),
    }
}

/// `b64 <text>`: decoded, when the text is valid base64 of valid UTF-8, then encoded.
///
/// Both, because `b64 test` is ambiguous — "test" is itself valid base64 — and guessing the
/// direction wrong silently hands back garbage. The decode row comes first since decoding a
/// secret out of a Kubernetes manifest is the common case.
fn base64_tool(query: &str) -> Option<Vec<ToolRow>> {
    let payload = after_keyword(query, "b64")?;
    let mut rows = Vec::new();
    if let Some(text) = base64_decode(payload).and_then(|b| String::from_utf8(b).ok()) {
        rows.push(ToolRow::new(text, "decoded"));
    }
    rows.push(ToolRow::new(base64_encode(payload.as_bytes()), "encoded"));
    Some(rows)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(B64[((n >> shift) & 0x3F) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard or URL-safe alphabet, padding optional, whitespace ignored.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for c in input.chars().filter(|c| !c.is_whitespace()) {
        if c == '=' {
            break;
        }
        let value = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    // Six leftover bits can only come from a truncated quantum — not real base64.
    (count < 6 && !out.is_empty()).then_some(out)
}

/// `url <text>`: decoded if it contains percent-escapes, then encoded.
fn url_tool(query: &str) -> Option<Vec<ToolRow>> {
    let payload = after_keyword(query, "url")?;
    let mut rows = Vec::new();
    if payload.contains('%')
        && let Some(text) = percent_decode(payload)
    {
        rows.push(ToolRow::new(text, "decoded"));
    }
    rows.push(ToolRow::new(percent_encode(payload), "encoded"));
    Some(rows)
}

/// Only the RFC 3986 unreserved set survives unescaped.
fn percent_encode(input: &str) -> String {
    input
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `None` on a malformed escape or a result that is not UTF-8. `+` is left alone: it means
/// a space only in form-encoded query strings, and guessing that would corrupt paths.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = input.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------

/// `hex <expression>`: the integer result in hex and binary.
fn hex_tool(query: &str) -> Option<Vec<ToolRow>> {
    let payload = after_keyword(query, "hex")?;
    let value = crate::calc::evaluate_value(payload)?;
    integer_rows(value, false)
}

/// A lone `0xff`: its decimal and binary. Arithmetic with hex literals goes through the
/// calculator, which accepts `0x` numbers directly.
fn hex_literal(query: &str) -> Option<Vec<ToolRow>> {
    let digits = query
        .strip_prefix("0x")
        .or_else(|| query.strip_prefix("0X"))?;
    let value = u64::from_str_radix(digits, 16).ok()?;
    integer_rows(value as f64, true)
}

fn integer_rows(value: f64, from_hex: bool) -> Option<Vec<ToolRow>> {
    if value < 0.0 || value.fract() != 0.0 || value > u64::MAX as f64 {
        return None;
    }
    let n = value as u64;
    let mut rows = Vec::new();
    if from_hex {
        rows.push(ToolRow::new(n.to_string(), "decimal"));
    } else {
        rows.push(ToolRow::new(format!("0x{n:x}"), "hex"));
    }
    rows.push(ToolRow::new(format!("0b{n:b}"), "binary"));
    Some(rows)
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// `1500MB`, `90s`, `1.8m`, `70kg`, `5'11"`, `1h 30m`: the value in the other forms that
/// matter. For a length or a weight that means the other system — metric in, imperial out,
/// and the reverse.
fn units(query: &str) -> Option<Vec<ToolRow>> {
    let normalized = normalize_units(query);
    let (source, target) = explicit_target(&normalized);
    let pairs = pairs(source)?;
    let mut readings: Vec<Vec<Unit>> = Vec::with_capacity(pairs.len());
    for (i, (_, name)) in pairs.iter().enumerate() {
        let options = if name.is_empty() {
            // `5'11`: a bare number straight after feet is inches, the way heights are written.
            let after_feet = i > 0 && i + 1 == pairs.len() && is_feet(pairs[i - 1].1);
            if !after_feet {
                return None;
            }
            readings_of("in")
        } else {
            readings_of(name)
        };
        if options.is_empty() {
            return None;
        }
        readings.push(options);
    }

    // Every dimension all the pairs can be read in, in the first pair's order of preference.
    // Usually one; a lone `m` is two, and gets both sets of rows.
    let mut rows = Vec::new();
    for dimension in readings[0].iter().map(|u| u.dimension) {
        let Some(units) = readings
            .iter()
            .map(|options| options.iter().copied().find(|u| u.dimension == dimension))
            .collect::<Option<Vec<Unit>>>()
        else {
            continue;
        };
        // Offsets and reciprocal scales do not add up (`5c 3f` means nothing), and only a
        // temperature is meaningfully below zero.
        if pairs.len() > 1 && units.iter().any(|u| !u.linear()) {
            continue;
        }
        if dimension != Dimension::Temperature && pairs.iter().any(|(n, _)| *n < 0.0) {
            continue;
        }
        let total: f64 = pairs.iter().zip(&units).map(|((n, _), u)| u.to_base(*n)).sum();
        if !total.is_finite() {
            continue;
        }
        if let Some((typed, choices)) = &target {
            if let Some(unit) = choices.iter().find(|u| u.dimension == dimension) {
                rows.push(target_row(total, *unit, typed));
            }
            continue;
        }
        let from_imperial = units.iter().all(|u| u.imperial);
        let from_metric = units.iter().all(|u| !u.imperial);
        rows.extend(match dimension {
            Dimension::Bytes => byte_rows(total),
            Dimension::Duration => duration_rows(total),
            Dimension::Length => converted(
                total,
                from_imperial,
                from_metric,
                imperial_length,
                metric_length,
            ),
            Dimension::Mass => converted(
                total,
                from_imperial,
                from_metric,
                imperial_mass,
                metric_mass,
            ),
            Dimension::DataRate => data_rate_rows(total, &units),
            other => display_rows(other, total, &units),
        });
    }
    (!rows.is_empty()).then_some(rows)
}

fn time_zones(query: &str) -> Option<Vec<ToolRow>> {
    timezones::evaluate(query)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dimension {
    Bytes,
    Duration,
    Length,
    Mass,
    Temperature,
    Volume,
    Area,
    Speed,
    Pressure,
    Energy,
    Power,
    Angle,
    Frequency,
    DataRate,
    Fuel,
}

/// A unit's size in its dimension's base, and whether it is imperial, which decides which way a
/// length, weight, volume or area converts. Bases: bytes, seconds, metres, kilograms, kelvin,
/// litres, square metres, metres per second, pascals, joules, watts, radians, hertz, bits per
/// second and litres per 100 km. For data rates `imperial` marks bytes per second.
#[derive(Clone, Copy, Debug)]
struct Unit {
    dimension: Dimension,
    factor: f64,
    imperial: bool,
    /// Added after scaling: only temperatures have one.
    offset: f64,
    /// `base = factor / amount`: fuel economy in mpg or km/L against L/100 km.
    inverse: bool,
}

impl Unit {
    fn to_base(self, amount: f64) -> f64 {
        if self.inverse {
            self.factor / amount
        } else {
            amount * self.factor + self.offset
        }
    }

    fn in_unit(self, base: f64) -> f64 {
        if self.inverse {
            self.factor / base
        } else {
            (base - self.offset) / self.factor
        }
    }

    fn linear(self) -> bool {
        self.offset == 0.0 && !self.inverse
    }

    fn same(self, other: Unit) -> bool {
        self.dimension == other.dimension
            && self.factor == other.factor
            && self.offset == other.offset
            && self.inverse == other.inverse
    }
}

/// Every reading of a unit name. One, except for `m`: metres and minutes are both `m`, and
/// a launcher cannot know which you meant. Metres come first — the SI meaning, and the one
/// a conversion is usually wanted for — with minutes after, so `90m` still says `1h 30m`.
fn readings_of(name: &str) -> Vec<Unit> {
    use Dimension::{
        Angle, Area, Bytes, DataRate, Duration, Energy, Frequency, Fuel, Length, Mass, Power,
        Pressure, Speed, Temperature, Volume,
    };
    let si = |dimension, factor| Unit {
        dimension,
        factor,
        imperial: false,
        offset: 0.0,
        inverse: false,
    };
    let imperial = |dimension, factor| Unit {
        dimension,
        factor,
        imperial: true,
        offset: 0.0,
        inverse: false,
    };
    // The US gallon is exactly 231 cubic inches; the smaller US measures divide it exactly.
    const GALLON: f64 = 3.785_411_784;
    let unit = match name.to_ascii_lowercase().as_str() {
        "m" => return vec![si(Length, 1.0), si(Duration, 60.0)],

        "b" | "byte" | "bytes" => si(Bytes, 1.0),
        "kb" => si(Bytes, 1e3),
        "mb" => si(Bytes, 1e6),
        "gb" => si(Bytes, 1e9),
        "tb" => si(Bytes, 1e12),
        "pb" => si(Bytes, 1e15),
        "kib" => si(Bytes, 1024f64.powi(1)),
        "mib" => si(Bytes, 1024f64.powi(2)),
        "gib" => si(Bytes, 1024f64.powi(3)),
        "tib" => si(Bytes, 1024f64.powi(4)),
        "pib" => si(Bytes, 1024f64.powi(5)),

        "ns" => si(Duration, 1e-9),
        // The micro sign and the Greek mu look identical and both get typed.
        "us" | "\u{b5}s" | "\u{3bc}s" => si(Duration, 1e-6),
        "ms" => si(Duration, 1e-3),
        "s" | "sec" | "secs" | "second" | "seconds" => si(Duration, 1.0),
        "min" | "mins" | "minute" | "minutes" => si(Duration, 60.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => si(Duration, 3_600.0),
        "d" | "day" | "days" => si(Duration, 86_400.0),

        "mm" | "millimeter" | "millimeters" | "millimetre" | "millimetres" => si(Length, 1e-3),
        "cm" | "centimeter" | "centimeters" | "centimetre" | "centimetres" => si(Length, 1e-2),
        "meter" | "meters" | "metre" | "metres" => si(Length, 1.0),
        "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => si(Length, 1e3),
        // Exact by definition since 1959: the inch is 25.4mm, and the rest follow from it.
        "in" | "inch" | "inches" | "\"" | "''" | "\u{201d}" | "\u{2033}" => {
            imperial(Length, 0.0254)
        }
        "ft" | "foot" | "feet" | "'" | "\u{2019}" | "\u{2032}" => imperial(Length, 0.3048),
        "yd" | "yard" | "yards" => imperial(Length, 0.9144),
        "mi" | "mile" | "miles" => imperial(Length, 1_609.344),

        "mg" | "milligram" | "milligrams" => si(Mass, 1e-6),
        "g" | "gram" | "grams" => si(Mass, 1e-3),
        "kg" | "kgs" | "kilo" | "kilos" | "kilogram" | "kilograms" => si(Mass, 1.0),
        "t" | "tonne" | "tonnes" => si(Mass, 1e3),
        // The pound is exactly 0.45359237kg, also by the 1959 agreement.
        "oz" | "ounce" | "ounces" => imperial(Mass, 0.453_592_37 / 16.0),
        "lb" | "lbs" | "pound" | "pounds" => imperial(Mass, 0.453_592_37),
        "st" | "stone" | "stones" => imperial(Mass, 0.453_592_37 * 14.0),

        // Not a lone `k`: `5k` means five thousand far more often than five kelvin.
        "c" | "\u{b0}c" | "celsius" => Unit {
            offset: 273.15,
            ..si(Temperature, 1.0)
        },
        "f" | "\u{b0}f" | "fahrenheit" => Unit {
            offset: 273.15 - 32.0 * 5.0 / 9.0,
            ..imperial(Temperature, 5.0 / 9.0)
        },
        "kelvin" | "kelvins" | "\u{b0}k" => si(Temperature, 1.0),

        "\u{b0}" | "deg" | "degree" | "degrees" => si(Angle, std::f64::consts::PI / 180.0),
        "rad" | "radian" | "radians" => si(Angle, 1.0),

        "ml" | "milliliter" | "milliliters" | "millilitre" | "millilitres" => si(Volume, 1e-3),
        "cl" => si(Volume, 1e-2),
        "dl" => si(Volume, 1e-1),
        "l" | "liter" | "liters" | "litre" | "litres" => si(Volume, 1.0),
        "tsp" | "teaspoon" | "teaspoons" => imperial(Volume, GALLON / 768.0),
        "tbsp" | "tablespoon" | "tablespoons" => imperial(Volume, GALLON / 256.0),
        "floz" => imperial(Volume, GALLON / 128.0),
        "cup" | "cups" => imperial(Volume, GALLON / 16.0),
        "pt" | "pint" | "pints" => imperial(Volume, GALLON / 8.0),
        "qt" | "quart" | "quarts" => imperial(Volume, GALLON / 4.0),
        "gal" | "gallon" | "gallons" => imperial(Volume, GALLON),

        "sqm" => si(Area, 1.0),
        "ha" | "hectare" | "hectares" => si(Area, 1e4),
        "sqkm" => si(Area, 1e6),
        "sqft" => imperial(Area, 0.092_903_04),
        "acre" | "acres" => imperial(Area, 4_046.856_422_4),
        "sqmi" => imperial(Area, 2_589_988.110_336),

        "kmh" => si(Speed, 1.0 / 3.6),
        "mps" => si(Speed, 1.0),
        "mph" => imperial(Speed, 0.447_04),
        "kn" | "kt" | "knot" | "knots" => si(Speed, 1_852.0 / 3_600.0),

        "pa" => si(Pressure, 1.0),
        "hpa" | "mbar" => si(Pressure, 100.0),
        "kpa" => si(Pressure, 1e3),
        "bar" => si(Pressure, 1e5),
        "atm" => si(Pressure, 101_325.0),
        "mmhg" => si(Pressure, 133.322_387_415),
        "psi" => imperial(Pressure, 6_894.757_293_168),

        "j" | "joule" | "joules" => si(Energy, 1.0),
        "kj" => si(Energy, 1e3),
        "cal" | "calorie" | "calories" => si(Energy, 4.184),
        "kcal" => si(Energy, 4_184.0),
        "wh" => si(Energy, 3_600.0),
        "kwh" => si(Energy, 3.6e6),
        "btu" => imperial(Energy, 1_055.055_852_62),

        "w" | "watt" | "watts" => si(Power, 1.0),
        "kw" => si(Power, 1e3),
        "hp" | "horsepower" => imperial(Power, 745.699_872),

        "hz" => si(Frequency, 1.0),
        "khz" => si(Frequency, 1e3),
        "mhz" => si(Frequency, 1e6),
        "ghz" => si(Frequency, 1e9),

        "bps" => si(DataRate, 1.0),
        "kbps" => si(DataRate, 1e3),
        "mbps" => si(DataRate, 1e6),
        "gbps" => si(DataRate, 1e9),
        "mbyteps" => imperial(DataRate, 8e6),
        "gbyteps" => imperial(DataRate, 8e9),

        // 235.214583… is 100 × the US gallon in litres ÷ the mile in kilometres.
        "lper100km" => si(Fuel, 1.0),
        "mpg" => Unit {
            inverse: true,
            ..imperial(Fuel, 100.0 * GALLON / 1.609_344)
        },
        "kml" => Unit {
            inverse: true,
            ..si(Fuel, 100.0)
        },

        _ => return Vec::new(),
    };
    vec![unit]
}

fn is_feet(name: &str) -> bool {
    readings_of(name)
        .iter()
        .any(|u| u.dimension == Dimension::Length && u.factor == 0.3048)
}

/// `5ft 11in`, `1h 30m`, `5'11"`, `3.5 GiB`, `-40c` as (amount, unit) pairs. `None` unless the
/// whole query is pairs, so `5 apples` and `4 in a row` stay searches.
fn pairs(query: &str) -> Option<Vec<(f64, &str)>> {
    let mut rest = query.trim();
    let mut out = Vec::new();
    while !rest.is_empty() {
        // Only the first amount may be negative: `-40c`, never `5 ft -3 in`.
        let negative = out.is_empty() && rest.starts_with('-');
        if negative {
            rest = &rest[1..];
        }
        let digits = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        let amount: f64 = rest[..digits].parse().ok()?;
        let amount = if negative { -amount } else { amount };
        rest = rest[digits..].trim_start();
        let unit_len = unit_len(rest);
        out.push((amount, &rest[..unit_len]));
        rest = rest[unit_len..].trim_start();
    }
    (!out.is_empty()).then_some(out)
}

/// A quote mark is a whole unit by itself, so `5'11"` splits; a degree sign takes the letters
/// after it (`°C`, or none for an angle); anything else runs to the end of the letters.
fn unit_len(s: &str) -> usize {
    if s.starts_with("''") {
        return 2;
    }
    match s.chars().next() {
        Some(c @ ('\'' | '"' | '\u{2019}' | '\u{2032}' | '\u{201d}' | '\u{2033}')) => c.len_utf8(),
        Some(degree @ '\u{b0}') => {
            let rest = &s[degree.len_utf8()..];
            degree.len_utf8() + rest.find(|c: char| !c.is_alphabetic()).unwrap_or(rest.len())
        }
        _ => s.find(|c: char| !c.is_alphabetic()).unwrap_or(s.len()),
    }
}

/// Unit names with spaces, slashes or superscripts, rewritten to the single words `readings_of`
/// knows, so the pair parser keeps its one rule: a number, then letters. Longer phrases come
/// first, so `sq mi` is not read as `sq m` followed by `i`.
const UNIT_PHRASES: &[(&str, &str)] = &[
    ("fluid ounces", "floz"),
    ("fluid ounce", "floz"),
    ("fl. oz", "floz"),
    ("fl oz", "floz"),
    ("square kilometres", "sqkm"),
    ("square kilometers", "sqkm"),
    ("square metres", "sqm"),
    ("square meters", "sqm"),
    ("square metre", "sqm"),
    ("square meter", "sqm"),
    ("square miles", "sqmi"),
    ("square mile", "sqmi"),
    ("square feet", "sqft"),
    ("square foot", "sqft"),
    ("sq km", "sqkm"),
    ("sq mi", "sqmi"),
    ("sq ft", "sqft"),
    ("sq m", "sqm"),
    ("km\u{b2}", "sqkm"),
    ("mi\u{b2}", "sqmi"),
    ("ft\u{b2}", "sqft"),
    ("m\u{b2}", "sqm"),
    ("km2", "sqkm"),
    ("mi2", "sqmi"),
    ("ft2", "sqft"),
    ("m2", "sqm"),
    ("kilometres per hour", "kmh"),
    ("kilometers per hour", "kmh"),
    ("miles per hour", "mph"),
    ("km/h", "kmh"),
    ("kph", "kmh"),
    ("m/s", "mps"),
    ("l/100 km", "lper100km"),
    ("l/100km", "lper100km"),
    ("km/l", "kml"),
    ("mb/s", "mbyteps"),
    ("gb/s", "gbyteps"),
];

fn normalize_units(query: &str) -> String {
    let mut text = query.trim().to_lowercase().replace('\u{ba}', "\u{b0}");
    for &(phrase, word) in UNIT_PHRASES {
        let mut from = 0;
        while let Some(found) = text.get(from..).and_then(|rest| rest.find(phrase)) {
            let start = from + found;
            let end = start + phrase.len();
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            let bounded = before.is_none_or(|c| c.is_whitespace() || c.is_ascii_digit() || c == '.')
                && after.is_none_or(|c| !c.is_alphanumeric());
            if bounded {
                text.replace_range(start..end, word);
                from = start + word.len();
            } else {
                from = end;
            }
        }
    }
    text
}

/// `5 km to miles`, `72f in c`: the amount before the last ` to ` or ` in `, and that unit. Only
/// when what follows is a unit, so `4 in a row` stays a search and `5 ft 11 in` stays a height.
fn explicit_target(query: &str) -> (&str, Option<(&str, Vec<Unit>)>) {
    let split = [" to ", " in "]
        .into_iter()
        .filter_map(|word| query.rfind(word).map(|at| (at, word.len())))
        .max_by_key(|&(at, _)| at);
    if let Some((at, len)) = split {
        let (amount, typed) = (query[..at].trim(), query[at + len..].trim());
        let choices = readings_of(typed);
        if !amount.is_empty() && !choices.is_empty() {
            return (amount, Some((typed, choices)));
        }
    }
    (query, None)
}

/// How each newer dimension is shown: (symbol, rail label, the name `readings_of` knows it by),
/// smallest first within each system.
fn display_units(dimension: Dimension) -> &'static [(&'static str, &'static str, &'static str)] {
    match dimension {
        Dimension::Temperature => &[
            ("\u{b0}C", "celsius", "c"),
            ("\u{b0}F", "fahrenheit", "f"),
            ("K", "kelvin", "kelvin"),
        ],
        Dimension::Volume => &[
            ("mL", "millilitres", "ml"),
            ("L", "litres", "l"),
            ("tsp", "teaspoons", "tsp"),
            ("tbsp", "tablespoons", "tbsp"),
            ("fl oz", "fluid oz", "floz"),
            ("cups", "US cups", "cup"),
            ("pt", "US pints", "pt"),
            ("qt", "US quarts", "qt"),
            ("gal", "US gallons", "gal"),
        ],
        Dimension::Area => &[
            ("m\u{b2}", "sq metres", "sqm"),
            ("ha", "hectares", "ha"),
            ("km\u{b2}", "sq km", "sqkm"),
            ("sq ft", "sq feet", "sqft"),
            ("acres", "acres", "acre"),
            ("sq mi", "sq miles", "sqmi"),
        ],
        Dimension::Speed => &[
            ("km/h", "km per hour", "kmh"),
            ("mph", "miles per hour", "mph"),
            ("m/s", "m per second", "mps"),
            ("kn", "knots", "kn"),
        ],
        Dimension::Pressure => &[
            ("kPa", "kilopascals", "kpa"),
            ("bar", "bar", "bar"),
            ("psi", "psi", "psi"),
            ("atm", "atmospheres", "atm"),
        ],
        Dimension::Energy => &[
            ("kJ", "kilojoules", "kj"),
            ("kWh", "kilowatt hours", "kwh"),
            ("kcal", "food calories", "kcal"),
            ("J", "joules", "j"),
            ("cal", "calories", "cal"),
            ("BTU", "BTU", "btu"),
        ],
        Dimension::Power => &[
            ("kW", "kilowatts", "kw"),
            ("hp", "horsepower", "hp"),
            ("W", "watts", "w"),
        ],
        Dimension::Angle => &[("\u{b0}", "degrees", "deg"), ("rad", "radians", "rad")],
        Dimension::Frequency => &[
            ("Hz", "hertz", "hz"),
            ("kHz", "kilohertz", "khz"),
            ("MHz", "megahertz", "mhz"),
            ("GHz", "gigahertz", "ghz"),
        ],
        Dimension::Fuel => &[
            ("L/100 km", "fuel use", "lper100km"),
            ("mpg", "US mpg", "mpg"),
            ("km/L", "km per litre", "kml"),
        ],
        Dimension::Bytes
        | Dimension::Duration
        | Dimension::Length
        | Dimension::Mass
        | Dimension::DataRate => &[],
    }
}

/// Volume and area convert to the other system, in the largest unit that is at least one and
/// the next one up when it is still a useful fraction (`2 L` is `2.11 qt` and `0.528 gal`).
/// Temperature shows the other two scales; frequency its natural scale; the rest the first two
/// listed units at a readable size.
fn display_rows(dimension: Dimension, base: f64, sources: &[Unit]) -> Vec<ToolRow> {
    let shown: Vec<(&str, &str, Unit, f64)> = display_units(dimension)
        .iter()
        .filter_map(|&(symbol, label, name)| {
            let unit = readings_of(name).into_iter().find(|u| u.dimension == dimension)?;
            let value = unit.in_unit(base);
            (!sources.iter().any(|s| s.same(unit)) && value.is_finite())
                .then_some((symbol, label, unit, value))
        })
        .collect();
    let row = |&(symbol, label, _, value): &(&str, &str, Unit, f64)| {
        ToolRow::new(quantity(value, symbol), label)
    };
    match dimension {
        Dimension::Volume | Dimension::Area => {
            let from_imperial = sources.iter().all(|u| u.imperial);
            let other: Vec<_> = shown
                .iter()
                .filter(|(_, _, unit, _)| unit.imperial != from_imperial)
                .collect();
            let best = other
                .iter()
                .rposition(|(_, _, _, value)| value.abs() >= 1.0)
                .unwrap_or(0);
            let mut rows: Vec<ToolRow> = other.get(best).map(|shown| row(shown)).into_iter().collect();
            if let Some(next) = other.get(best + 1).filter(|(_, _, _, value)| value.abs() >= 0.1) {
                rows.push(row(next));
            }
            rows
        }
        Dimension::Temperature => shown.iter().take(2).map(row).collect(),
        Dimension::Frequency => shown
            .iter()
            .filter(|(_, _, _, value)| (1.0..1e6).contains(&value.abs()))
            .take(1)
            .map(row)
            .collect(),
        _ => shown
            .iter()
            .filter(|(_, _, _, value)| *value == 0.0 || (1e-3..1e9).contains(&value.abs()))
            .take(2)
            .map(row)
            .collect(),
    }
}

/// The single row for `… to unit`: the unit's own symbol when it has one, otherwise the unit as
/// typed (`3.11 miles`).
fn target_row(base: f64, unit: Unit, typed: &str) -> ToolRow {
    let value = unit.in_unit(base);
    let known = display_units(unit.dimension).iter().find(|(_, _, name)| {
        readings_of(name).into_iter().any(|candidate| candidate.same(unit))
    });
    match known {
        Some(&(symbol, label, _)) => ToolRow::new(quantity(value, symbol), label),
        None => ToolRow::new(format!("{} {typed}", sig(value)), typed),
    }
}

/// A line speed and a transfer speed are the same number eight times over, so each converts
/// to the other, with how long a gigabyte takes at that rate.
fn data_rate_rows(bits: f64, sources: &[Unit]) -> Vec<ToolRow> {
    if bits <= 0.0 {
        return Vec::new();
    }
    let speed = if sources.iter().all(|u| u.imperial) {
        ToolRow::new(
            scaled(bits, 1000.0, &["bps", "Kbps", "Mbps", "Gbps", "Tbps"]),
            "line speed",
        )
    } else {
        ToolRow::new(
            scaled(bits / 8.0, 1000.0, &["B/s", "KB/s", "MB/s", "GB/s", "TB/s"]),
            "transfer",
        )
    };
    vec![
        speed,
        ToolRow::new(format!("1 GB in {}", human_duration(8e9 / bits)), "per GB"),
    ]
}

fn quantity(value: f64, symbol: &str) -> String {
    if symbol == "\u{b0}" {
        format!("{}\u{b0}", sig(value))
    } else {
        format!("{} {symbol}", sig(value))
    }
}

/// Enough digits to be useful at any size: one decimal from 100, two from 1, and at least two
/// significant digits below 1, so `0.00405 km²` does not round to zero.
fn sig(value: f64) -> String {
    let size = value.abs();
    let decimals = if size == 0.0 || size >= 100.0 {
        1
    } else if size >= 1.0 {
        2
    } else {
        (2 - size.log10().floor() as i32).clamp(2, 6) as usize
    };
    let formatted = format!("{value:.decimals$}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" { "0".to_owned() } else { trimmed.to_owned() }
}

/// Rows in the system the value was not given in; both, for a mix like `1ft 10cm`.
fn converted(
    base: f64,
    from_imperial: bool,
    from_metric: bool,
    imperial: fn(f64) -> Vec<ToolRow>,
    metric: fn(f64) -> Vec<ToolRow>,
) -> Vec<ToolRow> {
    let mut rows = Vec::new();
    if !from_imperial {
        rows.extend(imperial(base));
    }
    if !from_metric {
        rows.extend(metric(base));
    }
    rows
}

/// The unit a person would say at each scale: inches under a foot, feet and inches for a
/// height, feet up to a mile, then miles.
fn imperial_length(metres: f64) -> Vec<ToolRow> {
    let inches = metres / 0.0254;
    if inches < 12.0 {
        vec![ToolRow::new(format!("{} in", trim(inches)), "inches")]
    } else if inches < 120.0 {
        vec![
            ToolRow::new(feet_and_inches(inches), "feet and inches"),
            ToolRow::new(format!("{} in", trim(inches)), "inches"),
        ]
    } else if metres < 1_609.344 {
        vec![
            ToolRow::new(format!("{} ft", trim(inches / 12.0)), "feet"),
            ToolRow::new(format!("{} yd", trim(inches / 36.0)), "yards"),
        ]
    } else {
        vec![
            ToolRow::new(format!("{} mi", trim(metres / 1_609.344)), "miles"),
            ToolRow::new(format!("{} ft", trim(inches / 12.0)), "feet"),
        ]
    }
}

/// `5 ft 10.9 in`. Rounded as a whole first, so 71.97in reads `6 ft`, never `5 ft 12 in`.
fn feet_and_inches(inches: f64) -> String {
    let tenths = (inches * 10.0).round();
    let feet = (tenths / 120.0).floor();
    let rest = (tenths - feet * 120.0) / 10.0;
    if rest == 0.0 {
        format!("{feet} ft")
    } else {
        format!("{feet} ft {} in", trim(rest))
    }
}

fn metric_length(metres: f64) -> Vec<ToolRow> {
    if metres < 0.01 {
        vec![ToolRow::new(
            format!("{} mm", trim(metres * 1e3)),
            "millimetres",
        )]
    } else if metres < 1.0 {
        vec![ToolRow::new(
            format!("{} cm", trim(metres * 1e2)),
            "centimetres",
        )]
    } else if metres < 1_000.0 {
        vec![
            ToolRow::new(format!("{} m", trim(metres)), "metres"),
            ToolRow::new(format!("{} cm", trim(metres * 1e2)), "centimetres"),
        ]
    } else {
        vec![
            ToolRow::new(format!("{} km", trim(metres / 1e3)), "kilometres"),
            ToolRow::new(format!("{} m", trim(metres)), "metres"),
        ]
    }
}

fn imperial_mass(kg: f64) -> Vec<ToolRow> {
    let pounds = kg / 0.453_592_37;
    if pounds < 1.0 {
        vec![ToolRow::new(
            format!("{} oz", trim(pounds * 16.0)),
            "ounces",
        )]
    } else {
        vec![
            ToolRow::new(format!("{} lb", trim(pounds)), "pounds"),
            ToolRow::new(pounds_and_ounces(pounds), "pounds and ounces"),
        ]
    }
}

/// `154 lb 5.2 oz`, rounded as a whole for the same reason as [`feet_and_inches`].
fn pounds_and_ounces(pounds: f64) -> String {
    let tenths = (pounds * 160.0).round();
    let whole = (tenths / 160.0).floor();
    let rest = (tenths - whole * 160.0) / 10.0;
    if rest == 0.0 {
        format!("{whole} lb")
    } else {
        format!("{whole} lb {} oz", trim(rest))
    }
}

fn metric_mass(kg: f64) -> Vec<ToolRow> {
    if kg < 1e-3 {
        vec![ToolRow::new(format!("{} mg", trim(kg * 1e6)), "milligrams")]
    } else if kg < 1.0 {
        vec![ToolRow::new(format!("{} g", trim(kg * 1e3)), "grams")]
    } else if kg < 1_000.0 {
        vec![ToolRow::new(format!("{} kg", trim(kg)), "kilograms")]
    } else {
        vec![
            ToolRow::new(format!("{} t", trim(kg / 1e3)), "tonnes"),
            ToolRow::new(format!("{} kg", trim(kg)), "kilograms"),
        ]
    }
}

fn byte_rows(bytes: f64) -> Vec<ToolRow> {
    vec![
        ToolRow::new(
            scaled(bytes, 1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB"]),
            "binary",
        ),
        ToolRow::new(
            scaled(bytes, 1000.0, &["B", "KB", "MB", "GB", "TB", "PB"]),
            "decimal",
        ),
        ToolRow::new(format!("{}", bytes.round() as u64), "bytes"),
    ]
}

/// The largest unit the value is at least one of, to two decimals with trailing zeros cut.
fn scaled(value: f64, base: f64, names: &[&str]) -> String {
    let mut v = value;
    let mut i = 0;
    while v >= base && i + 1 < names.len() {
        v /= base;
        i += 1;
    }
    format!("{} {}", trim(v), names[i])
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_owned()
}

fn duration_rows(secs: f64) -> Vec<ToolRow> {
    vec![
        ToolRow::new(human_duration(secs), "duration"),
        ToolRow::new(trim(secs), "seconds"),
        ToolRow::new(trim(secs * 1_000.0), "milliseconds"),
    ]
}

/// `1d 2h 3m 4s`, zero parts dropped; sub-second remainders shown in ms.
fn human_duration(secs: f64) -> String {
    let total_ms = (secs * 1_000.0).round() as u64;
    let (mut rest, ms) = (total_ms / 1_000, total_ms % 1_000);
    let mut parts = Vec::new();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if rest >= size {
            parts.push(format!("{}{unit}", rest / size));
            rest %= size;
        }
    }
    if ms > 0 || parts.is_empty() {
        parts.push(format!("{ms}ms"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        // FIPS 180-4 and the two everyone checks against.
        let hex = |text: &str| {
            sha256(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(
            hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        // Longer than one 64-byte block, so the multi-chunk path is covered too.
        assert_eq!(
            hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_gives_hex_and_base64() {
        let rows = evaluate("sha256 hello");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].value,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(rows[0].detail, "SHA-256");
        assert_eq!(
            rows[1].value,
            "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ="
        );
        // A bare keyword is not a request to hash nothing.
        assert!(evaluate("sha256").is_empty());
        assert!(evaluate("sha256 ").is_empty());
    }

    #[test]
    fn json_formats_then_flattens() {
        // Deliberately not alphabetical, so a reordering formatter would fail this.

        let rows = evaluate(r#"json {"b":1,"a":[2,3]}"#);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].value.contains('\n'),
            "the first row is the formatted one"
        );
        assert_eq!(rows[0].detail, "formatted");
        // Key order survives: `serde_json`'s `preserve_order` is on precisely so a
        // formatter cannot quietly sort your config out of the order you wrote it in.
        assert_eq!(rows[1].value, r#"{"b":1,"a":[2,3]}"#);
        assert_eq!(rows[1].detail, "one line");
    }

    #[test]
    fn json_that_will_not_parse_says_where() {
        let rows = evaluate("json {oops");
        assert_eq!(
            rows.len(),
            1,
            "one row, not a fall-through to an app search"
        );
        assert_eq!(rows[0].value, "{oops", "Enter hands back what was given");
        assert!(
            rows[0].detail.starts_with("not JSON — "),
            "{:?}",
            rows[0].detail
        );
        assert!(rows[0].detail.contains("line 1"), "{:?}", rows[0].detail);
    }

    #[test]
    fn a_scalar_is_still_json() {
        assert_eq!(evaluate("json 42")[1].value, "42");
        assert_eq!(evaluate(r#"json "hi""#)[1].value, r#""hi""#);
    }

    fn values(q: &str) -> Vec<String> {
        evaluate(q).into_iter().map(|r| r.value).collect()
    }

    #[test]
    fn epochs_in_seconds_and_milliseconds() {
        assert_eq!(
            values("@1757548800"),
            ["2025-09-11 00:00:00 UTC", "2025-09-11T00:00:00Z"]
        );
        assert_eq!(values("@1757548800123")[0], "2025-09-11 00:00:00 UTC");
        let rows = evaluate("@1757548800");
        assert_eq!(rows[0].timestamp, 1_757_548_800, "Swift gets the epoch");
        assert_eq!(rows[1].timestamp, 0, "and renders it once");
        assert!(evaluate("@").is_empty() && evaluate("@abc").is_empty());
    }

    #[test]
    fn now_gives_seconds_and_milliseconds() {
        let rows = values("now");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 10, "a 2020s epoch has ten digits");
        assert_eq!(rows[1].len(), 13);
    }

    #[test]
    fn uuids_are_well_formed_v4_and_random() {
        let a = values("uuid");
        let b = values("UUID");
        assert_eq!(a[0].len(), 36);
        assert_eq!(&a[0][14..15], "4", "version nibble");
        assert!(
            matches!(&a[0][19..20], "8" | "9" | "a" | "b"),
            "variant bits"
        );
        assert_ne!(a, b, "two calls, two ids");
        assert_eq!(
            format_uuid_v4([0; 16]),
            "00000000-0000-4000-8000-000000000000"
        );
    }

    #[test]
    fn base64_decodes_when_it_can_and_always_encodes() {
        assert_eq!(values("b64 SGVsbG8="), ["Hello", "U0dWc2JHOD0="]);
        assert_eq!(
            values("b64 SGVsbG8"),
            ["Hello", "U0dWc2JHOA=="],
            "padding optional"
        );
        assert_eq!(
            values("b64 héllo!")[0],
            "aMOpbGxvIQ==",
            "not base64: encode only"
        );
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_decode("Zm9v"), Some(b"foo".to_vec()));
        assert_eq!(
            base64_decode("-_8"),
            base64_decode("+/8"),
            "URL-safe alphabet"
        );
    }

    #[test]
    fn url_decodes_and_encodes() {
        assert_eq!(values("url a%20b%2Fc"), ["a b/c", "a%2520b%252Fc"]);
        assert_eq!(values("url a b"), ["a%20b"], "nothing to decode");
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(
            percent_decode("a+b"),
            Some("a+b".into()),
            "+ is not a space here"
        );
    }

    #[test]
    fn hex_both_ways() {
        assert_eq!(values("0xff"), ["255", "0b11111111"]);
        assert_eq!(values("hex 255"), ["0xff", "0b11111111"]);
        assert_eq!(values("hex 256 + 1")[0], "0x101");
        assert!(
            evaluate("0xff + 1").is_empty(),
            "arithmetic is the calculator's"
        );
        assert_eq!(crate::calc::evaluate("0xff + 1"), Some(256.0));
        assert!(evaluate("hex -1").is_empty() && evaluate("hex 1.5").is_empty());
    }

    #[test]
    fn byte_units_show_binary_decimal_and_raw() {
        assert_eq!(values("1500MB"), ["1.4 GiB", "1.5 GB", "1500000000"]);
        assert_eq!(values("3.5 GiB"), ["3.5 GiB", "3.76 GB", "3758096384"]);
        assert_eq!(values("512b")[2], "512");
    }

    #[test]
    fn durations_show_human_seconds_and_ms() {
        assert_eq!(values("90s"), ["1m 30s", "90", "90000"]);
        assert_eq!(values("3600000ms"), ["1h", "3600", "3600000"]);
        assert_eq!(values("1.5h")[0], "1h 30m");
        assert_eq!(values("250ms")[0], "250ms");
        assert_eq!(
            values("1h 30m"),
            ["1h 30m", "5400", "5400000"],
            "compound, m as minutes"
        );
    }

    #[test]
    fn lengths_convert_to_the_other_system() {
        assert_eq!(values("180cm"), ["5 ft 10.9 in", "70.87 in"]);
        assert_eq!(values("5'11\""), ["1.8 m", "180.34 cm"]);
        assert_eq!(
            values("5'11"),
            values("5'11\""),
            "a bare number after feet is inches"
        );
        assert_eq!(values("5 ft 11 in"), values("5'11\""));
        assert_eq!(
            values("5\u{2019}11\u{201d}"),
            values("5'11\""),
            "curly quotes, as pasted"
        );
        assert_eq!(values("6'"), ["1.83 m", "182.88 cm"]);
        assert_eq!(values("5km"), ["3.11 mi", "16404.2 ft"]);
        assert_eq!(values("10 miles"), ["16.09 km", "16093.44 m"]);
        assert_eq!(values("30 ft"), ["9.14 m", "914.4 cm"]);
        assert_eq!(values("2.5cm"), ["0.98 in"]);
        assert_eq!(
            values("1ft 10cm"),
            ["1 ft 3.9 in", "15.94 in", "40.48 cm"],
            "mixed: both"
        );
    }

    #[test]
    fn a_lone_m_is_metres_then_minutes() {
        assert_eq!(
            values("100m"),
            ["328.08 ft", "109.36 yd", "1h 40m", "6000", "6000000"]
        );
        assert_eq!(values("1.8m")[0], "5 ft 10.9 in");
        assert_eq!(
            values("1m 80cm"),
            ["5 ft 10.9 in", "70.87 in"],
            "cm makes it a length"
        );
    }

    #[test]
    fn weights_convert_to_the_other_system() {
        assert_eq!(values("70kg"), ["154.32 lb", "154 lb 5.2 oz"]);
        assert_eq!(values("150 lbs"), ["68.04 kg"]);
        assert_eq!(values("8 oz"), ["226.8 g"]);
        assert_eq!(values("500g"), ["1.1 lb", "1 lb 1.6 oz"]);
        assert_eq!(values("10 st"), ["63.5 kg"]);
        assert_eq!(values("2t"), ["4409.25 lb", "4409 lb 3.9 oz"]);
        assert_eq!(values("1 lb 8 oz"), ["680.39 g"]);
    }

    #[test]
    fn rounding_carries_into_the_larger_unit() {
        assert_eq!(feet_and_inches(71.97), "6 ft");
        assert_eq!(pounds_and_ounces(1.999_99), "2 lb");
    }

    #[test]
    fn temperatures_volumes_and_areas_convert() {
        assert_eq!(values("72f"), ["22.22 \u{b0}C", "295.4 K"]);
        assert_eq!(values("-40c")[0], "-40 \u{b0}F");
        assert_eq!(values("100 \u{b0}C")[0], "212 \u{b0}F");
        assert_eq!(values("300 kelvin"), ["26.85 \u{b0}C", "80.33 \u{b0}F"]);
        assert_eq!(values("2 L"), ["2.11 qt", "0.528 gal"]);
        assert_eq!(values("1 cup"), ["236.6 mL", "0.237 L"]);
        assert_eq!(values("12 fl oz"), ["354.9 mL", "0.355 L"]);
        assert_eq!(values("1 acre"), ["4046.9 m\u{b2}", "0.405 ha"]);
        assert_eq!(values("1000 sq ft"), ["92.9 m\u{b2}"]);
    }

    #[test]
    fn speeds_pressures_energy_power_angles_rates_and_fuel_convert() {
        assert_eq!(values("100 km/h"), ["62.14 mph", "27.78 m/s"]);
        assert_eq!(values("60 mph"), ["96.56 km/h", "26.82 m/s"]);
        assert_eq!(values("20 knots"), ["37.04 km/h", "23.02 mph"]);
        assert_eq!(values("32 psi"), ["220.6 kPa", "2.21 bar"]);
        assert_eq!(values("500 kcal"), ["2092 kJ", "0.581 kWh"]);
        assert_eq!(values("150 hp")[0], "111.9 kW");
        assert_eq!(values("90\u{b0}"), ["1.57 rad"]);
        assert_eq!(values("2.4 ghz"), ["2400 MHz"]);
        assert_eq!(values("100 mbps"), ["12.5 MB/s", "1 GB in 1m 20s"]);
        assert_eq!(values("50 MB/s"), ["400 Mbps", "1 GB in 20s"]);
        assert_eq!(values("30 mpg"), ["7.84 L/100 km", "12.75 km/L"]);
    }

    #[test]
    fn to_and_in_choose_the_unit() {
        assert_eq!(values("5 km to miles"), ["3.11 miles"]);
        assert_eq!(values("72f in c"), ["22.22 \u{b0}C"]);
        assert_eq!(values("1.5 cups to ml"), ["354.9 mL"]);
        assert_eq!(values("90 min to hours"), ["1.5 hours"]);
        assert_eq!(values("10 in in cm"), ["25.4 cm"]);
        assert_eq!(values("5 ft 11 in to cm"), ["180.3 cm"]);
        assert!(evaluate("5 km to paris").is_empty());
        assert!(evaluate("3 cups of flour").is_empty());
    }

    #[test]
    fn names_and_plain_words_never_trigger_a_tool() {
        for q in [
            "12 * 34",
            "Safari",
            "hex",
            "b64",
            "url",
            "90",
            "now please",
            "uuid4",
            "5 apples",
            "4 in a row",
            "5 5",
            "m 5",
            "@now",
            "",
        ] {
            assert!(evaluate(q).is_empty(), "{q:?} should not produce tool rows");
        }
    }
}
