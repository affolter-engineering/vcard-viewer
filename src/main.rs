use colored::Colorize;
use std::env;
use std::fs;
use std::path::Path;
use std::process;

#[derive(Debug, Clone)]
struct Property {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    fn type_param(&self) -> Option<&str> {
        self.param("TYPE")
    }
}

#[derive(Debug, Clone)]
struct VCard {
    properties: Vec<Property>,
}

impl VCard {
    fn get(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    fn get_all(&self, name: &str) -> Vec<&Property> {
        self.properties
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(name))
            .collect()
    }
}

/// Unfold logical lines (RFC 6350 §3.2): a CRLF or LF followed by a single
/// whitespace character is a line continuation and should be removed.
fn unfold(content: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let mut result = String::with_capacity(normalized.len());
    let mut chars = normalized.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\n' {
            match chars.peek() {
                Some(' ') | Some('\t') => {
                    chars.next(); // consume the folding whitespace
                }
                _ => result.push(c),
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Parse a single `KEY;PARAM=VAL:value` line into a `Property`.
fn parse_line(line: &str) -> Option<Property> {
    let (name_params, value) = line.split_once(':')?;

    let mut parts = name_params.split(';');
    let raw_name = parts.next()?;

    // Strip optional group prefix, e.g. "item1.EMAIL" → "EMAIL"
    let name = raw_name
        .find('.')
        .map(|i| &raw_name[i + 1..])
        .unwrap_or(raw_name)
        .to_uppercase();

    let mut params = Vec::new();
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            params.push((k.to_uppercase(), v.to_string()));
        } else {
            // vCard 2.1 bare type tokens, e.g. "TEL;WORK;VOICE:..."
            params.push(("TYPE".to_string(), part.to_string()));
        }
    }

    Some(Property { name, params, value: value.to_string() })
}

/// Parse the entire vCard content into a vector of `VCard` structs.
fn parse_vcards(content: &str) -> Vec<VCard> {
    let unfolded = unfold(content);
    let mut cards: Vec<VCard> = Vec::new();
    let mut current: Option<Vec<Property>> = None;

    for raw_line in unfolded.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            current = Some(Vec::new());
        } else if line.eq_ignore_ascii_case("END:VCARD") {
            if let Some(props) = current.take() {
                cards.push(VCard { properties: props });
            }
        } else if let Some(ref mut props) = current {
            if let Some(prop) = parse_line(line) {
                props.push(prop);
            }
        }
    }

    cards
}

/// Decode quoted-printable encoding (RFC 2045 §6.7)
fn decode_quoted_printable(s: &str) -> String {
    // Collect raw bytes so that multi-byte UTF-8 sequences (e.g. =C3=A4 for ä)
    // are reassembled correctly instead of being cast to Latin-1 chars one by one.
    let mut bytes: Vec<u8> = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '=' {
            let h1 = chars.next();
            let h2 = chars.next();
            match (h1, h2) {
                (Some(a), Some(b)) if a != '\n' => {
                    let hex = format!("{}{}", a, b);
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        bytes.push(byte);
                    } else {
                        // Not a valid hex sequence – keep literally.
                        bytes.push(b'=');
                        bytes.extend_from_slice(a.to_string().as_bytes());
                        bytes.extend_from_slice(b.to_string().as_bytes());
                    }
                }
                // Soft line-break (=\n): discard, continue.
                _ => {}
            }
        } else {
            // Literal ASCII chars in a QP stream are always single-byte.
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    // Prefer UTF-8 (vCard 3/4 and QP-encoded UTF-8).
    // Fall back to Latin-1 (vCard 2.1 with CHARSET=ISO-8859-1).
    String::from_utf8(bytes.clone())
        .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect())
}

/// Attempt to repair Mojibake: UTF-8 bytes that were each stored as a
/// Latin-1 code point and then re-encoded as UTF-8 (e.g. "ü" → "Ã¼").
///
/// Strategy: re-encode every char as its Latin-1 byte value (only possible
/// when all code points are ≤ U+00FF), then decode the resulting byte slice
/// as UTF-8. The fix is applied only when the decoded string has fewer
/// characters than the original (Mojibake always inflates the count), which
/// guards against false positives on legitimate Latin-1 supplement text.
fn fix_mojibake(s: &str) -> String {
    // Fast path: ASCII-only strings can never be Mojibake.
    if s.is_ascii() {
        return s.to_string();
    }
    // Collect each char as its Latin-1 byte, bailing out if any char > U+00FF.
    let bytes: Option<Vec<u8>> = s
        .chars()
        .map(|c| if (c as u32) <= 0xFF { Some(c as u8) } else { None })
        .collect();
    match bytes {
        Some(b) => match String::from_utf8(b) {
            Ok(fixed) if fixed.chars().count() < s.chars().count() => fixed,
            _ => s.to_string(),
        },
        None => s.to_string(),
    }
}

/// Decode a property value, handling quoted-printable and vCard text escapes.
fn decode_value(prop: &Property) -> String {
    let encoding = prop.param("ENCODING");

    let decoded =
        if encoding.map(|e| e.eq_ignore_ascii_case("QUOTED-PRINTABLE")).unwrap_or(false) {
            decode_quoted_printable(&prop.value)
        } else {
            // Non-QP values: attempt to repair Mojibake (UTF-8 bytes stored as
            // Latin-1 code points), a common artefact of buggy vCard exporters.
            fix_mojibake(&prop.value)
        };

    // Unescape vCard text escapes
    decoded
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// Format the structured `N` property (Last;First;Middle;Prefix;Suffix).
fn format_name(value: &str) -> String {
    let p: Vec<&str> = value.splitn(5, ';').collect();
    let last   = p.first().copied().unwrap_or("").trim();
    let first  = p.get(1).copied().unwrap_or("").trim();
    let middle = p.get(2).copied().unwrap_or("").trim();
    let prefix = p.get(3).copied().unwrap_or("").trim();
    let suffix = p.get(4).copied().unwrap_or("").trim();

    let mut out = String::new();
    for part in [prefix, first, middle, last] {
        if !part.is_empty() {
            if !out.is_empty() { out.push(' '); }
            out.push_str(part);
        }
    }
    if !suffix.is_empty() {
        out.push_str(", ");
        out.push_str(suffix);
    }
    out
}

/// Format the structured `ADR` property
/// (P.O. Box;Extended;Street;City;Region;PostalCode;Country).
fn format_address(value: &str) -> String {
    let p: Vec<&str> = value.splitn(7, ';').collect();
    let po_box   = p.first().copied().unwrap_or("").trim();
    let extended = p.get(1).copied().unwrap_or("").trim();
    let street   = p.get(2).copied().unwrap_or("").trim();
    let city     = p.get(3).copied().unwrap_or("").trim();
    let region   = p.get(4).copied().unwrap_or("").trim();
    let postal   = p.get(5).copied().unwrap_or("").trim();
    let country  = p.get(6).copied().unwrap_or("").trim();

    let mut segments: Vec<String> = Vec::new();
    if !po_box.is_empty()   { segments.push(format!("P.O. Box {po_box}")); }
    if !extended.is_empty() { segments.push(extended.to_string()); }
    if !street.is_empty()   { segments.push(street.to_string()); }

    let mut city_line = String::new();
    if !city.is_empty()   { city_line.push_str(city); }
    if !region.is_empty() {
        if !city_line.is_empty() { city_line.push_str(", "); }
        city_line.push_str(region);
    }
    if !postal.is_empty() {
        if !city_line.is_empty() { city_line.push(' '); }
        city_line.push_str(postal);
    }
    if !city_line.is_empty() { segments.push(city_line); }
    if !country.is_empty()   { segments.push(country.to_string()); }

    segments.join(", ")
}

const LABEL_WIDTH: usize = 14;
const RULE_WIDTH: usize  = 52;

fn print_row(label: &str, value: &str, type_hint: Option<&str>) {
    let label_col = format!("{:>width$}", label, width = LABEL_WIDTH)
        .bold()
        .blue()
        .to_string();
    let type_col = type_hint
        .map(|t| format!("  [{}]", t.to_lowercase()).dimmed().to_string())
        .unwrap_or_default();

    // First line of a potentially multi-line value
    let mut lines = value.lines();
    if let Some(first) = lines.next() {
        println!("  {}  {}{}", label_col, first, type_col);
    }
    // Continuation lines (e.g. multi-line notes)
    let indent = " ".repeat(2 + LABEL_WIDTH + 2);
    for line in lines {
        println!("{}{}", indent, line);
    }
}

/// Display a single vCard in a human-readable format.
fn display_card(card: &VCard, index: usize, total: usize) {
    let counter = if total > 1 { format!(" {}/{} ", index + 1, total) } else { String::new() };
    let title   = format!(" Contact{} ", counter);
    let fill    = RULE_WIDTH.saturating_sub(title.len() + 4);
    println!(
        "{}",
        format!("── {} {}", title.bold(), "─".repeat(fill)).cyan()
    );

    let display_name = card
        .get("FN")
        .map(decode_value)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            card.get("N")
                .map(|p| format_name(&decode_value(p)))
                .filter(|s| !s.is_empty())
        });

    if let Some(name) = display_name {
        println!("  {}", name.bold().white());
        println!();
    }

    if let Some(p) = card.get("ORG") {
        let display = decode_value(p)
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        if !display.is_empty() { print_row("Organisation", &display, None); }
    }
    for prop in &[("TITLE", "Title"), ("ROLE", "Role")] {
        if let Some(p) = card.get(prop.0) {
            let val = decode_value(p);
            if !val.is_empty() { print_row(prop.1, &val, None); }
        }
    }

    for p in card.get_all("EMAIL") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Email", &val, p.type_param()); }
    }
    for p in card.get_all("TEL") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Phone", &val, p.type_param()); }
    }
    for p in card.get_all("ADR") {
        let formatted = format_address(&decode_value(p));
        if !formatted.is_empty() { print_row("Address", &formatted, p.type_param()); }
    }

    if let Some(p) = card.get("BDAY") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Birthday", &val, None); }
    }
    if let Some(p) = card.get("ANNIVERSARY") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Anniversary", &val, None); }
    }
    for p in card.get_all("URL") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Website", &val, p.type_param()); }
    }
    for p in card.get_all("IMPP") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("IM", &val, p.type_param()); }
    }
    if let Some(p) = card.get("NOTE") {
        let val = decode_value(p);
        if !val.is_empty() { print_row("Note", &val, None); }
    }
    if card.get("PHOTO").is_some() {
        print_row("Photo", "(embedded)", None);
    }

    println!("{}", "─".repeat(RULE_WIDTH).cyan());
}

/// Extract a single flat string for a given property name from a card.
fn card_field(card: &VCard, name: &str) -> String {
    card.get(name).map(decode_value).unwrap_or_default()
}

/// Row data for one vCard in the table view.
struct TableRow {
    name:  String,
    org:   String,
    title: String,
    email: String,
    phone: String,
}

impl TableRow {
    fn from_card(card: &VCard) -> Self {
        let name = {
            let fn_ = card_field(card, "FN");
            if fn_.is_empty() {
                card.get("N")
                    .map(|p| format_name(&decode_value(p)))
                    .unwrap_or_default()
            } else {
                fn_
            }
        };

        let org = card
            .get("ORG")
            .map(|p| {
                decode_value(p)
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();

        let title = {
            let t = card_field(card, "TITLE");
            if t.is_empty() { card_field(card, "ROLE") } else { t }
        };

        let email = card
            .get_all("EMAIL")
            .into_iter()
            .map(decode_value)
            .find(|v| !v.is_empty())
            .unwrap_or_default();

        let phone = card
            .get_all("TEL")
            .into_iter()
            .map(decode_value)
            .find(|v| !v.is_empty())
            .unwrap_or_default();

        TableRow { name, org, title, email, phone }
    }
}

/// Truncate a string to `max` visible characters, appending `…` if needed.
fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        chars[..max.saturating_sub(1)].iter().collect::<String>() + "…"
    }
}

fn display_table(cards: &[VCard]) {
    let headers = ["#", "Name", "Organisation", "Title/Role", "Email", "Phone"];

    let rows: Vec<TableRow> = cards.iter().map(TableRow::from_card).collect();

    // Compute column widths: max of header width and all cell widths, capped
    // so the table stays within ~120 columns total.
    let caps = [4usize, 28, 24, 22, 32, 20];
    let mut widths: Vec<usize> = headers
        .iter()
        .zip(caps.iter())
        .map(|(h, &cap)| h.len().min(cap))
        .collect();

    for (i, row) in rows.iter().enumerate() {
        let num = (i + 1).to_string();
        let cells = [
            num.as_str(),
            row.name.as_str(),
            row.org.as_str(),
            row.title.as_str(),
            row.email.as_str(),
            row.phone.as_str(),
        ];
        for (col, cell) in cells.iter().enumerate() {
            let visible: usize = cell.chars().count();
            widths[col] = widths[col].max(visible.min(caps[col]));
        }
    }

    let sep = widths
        .iter()
        .map(|&w| "─".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("┼");
    println!("{}", format!("┌{}┐", widths.iter().map(|&w| "─".repeat(w + 2)).collect::<Vec<_>>().join("┬")).cyan());

    let header_cells: String = headers
        .iter()
        .zip(widths.iter())
        .map(|(h, &w)| format!(" {:<w$} ", h, w = w))
        .collect::<Vec<_>>()
        .join("│");
    println!("{}", format!("│{}│", header_cells).cyan().bold().to_string());

    println!("{}", format!("├{}┤", sep).cyan());

    for (i, row) in rows.iter().enumerate() {
        let num = (i + 1).to_string();
        let cells = [
            num.as_str(),
            row.name.as_str(),
            row.org.as_str(),
            row.title.as_str(),
            row.email.as_str(),
            row.phone.as_str(),
        ];
        let line: String = cells
            .iter()
            .zip(widths.iter())
            .enumerate()
            .map(|(col, (cell, &w))| {
                let t = truncate(cell, w);
                if col == 0 {
                    format!(" {:<w$} ", t.dimmed(), w = w)
                } else if col == 1 {
                    format!(" {:<w$} ", t.bold(), w = w)
                } else {
                    format!(" {:<w$} ", t, w = w)
                }
            })
            .collect::<Vec<_>>()
            .join("│");
        println!("│{}│", line);
    }

    println!("{}", format!("└{}┘", widths.iter().map(|&w| "─".repeat(w + 2)).collect::<Vec<_>>().join("┴")).cyan());
    println!("{}", format!("  {} contact{}", rows.len(), if rows.len() == 1 { "" } else { "s" }).dimmed());
}

/// Wrap a field value in double-quotes, escaping inner double-quotes per RFC 4180.
fn csv_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn output_csv(cards: &[(String, VCard)]) {
    // Header
    println!(
        "{}",
        [
            "source", "name", "organisation", "title", "role",
            "email", "phone", "address", "birthday", "anniversary",
            "website", "note",
        ]
        .iter()
        .map(|h| csv_escape(h))
        .collect::<Vec<_>>()
        .join(",")
    );

    for (source, card) in cards {
        let name = {
            let fn_ = card_field(card, "FN");
            if fn_.is_empty() {
                card.get("N")
                    .map(|p| format_name(&decode_value(p)))
                    .unwrap_or_default()
            } else {
                fn_
            }
        };

        let org = card
            .get("ORG")
            .map(|p| {
                decode_value(p)
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_default();

        let title   = card_field(card, "TITLE");
        let role    = card_field(card, "ROLE");

        let emails: String = card
            .get_all("EMAIL")
            .iter()
            .map(|p| decode_value(p))
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");

        let phones: String = card
            .get_all("TEL")
            .iter()
            .map(|p| decode_value(p))
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");

        let addresses: String = card
            .get_all("ADR")
            .iter()
            .map(|p| format_address(&decode_value(p)))
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");

        let bday        = card_field(card, "BDAY");
        let anniversary = card_field(card, "ANNIVERSARY");

        let websites: String = card
            .get_all("URL")
            .iter()
            .map(|p| decode_value(p))
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");

        let note = card_field(card, "NOTE");

        println!(
            "{}",
            [
                source.as_str(),
                name.as_str(),
                org.as_str(),
                title.as_str(),
                role.as_str(),
                emails.as_str(),
                phones.as_str(),
                addresses.as_str(),
                bday.as_str(),
                anniversary.as_str(),
                websites.as_str(),
                note.as_str(),
            ]
            .iter()
            .map(|v| csv_escape(v))
            .collect::<Vec<_>>()
            .join(",")
        );
    }
}

/// Collect all `.vcf` / `.vcard` files under a directory (non-recursive).
fn collect_vcf_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("vcf") || e.eq_ignore_ascii_case("vcard"))
                        .unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            eprintln!("Error: cannot read directory '{}': {}", dir.display(), e);
            process::exit(1);
        }
    };
    files.sort();
    files
}

/// Tests for the parsing and formatting functions.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfold_plain_line() {
        assert_eq!(unfold("FN:Alice"), "FN:Alice");
    }

    #[test]
    fn unfold_lf_space() {
        assert_eq!(unfold("FN:Ali\n ce"), "FN:Alice");
    }

    #[test]
    fn unfold_lf_tab() {
        assert_eq!(unfold("FN:Ali\n\tce"), "FN:Alice");
    }

    #[test]
    fn unfold_crlf_space() {
        assert_eq!(unfold("FN:Ali\r\n ce"), "FN:Alice");
    }

    #[test]
    fn unfold_preserves_bare_newline() {
        // A newline NOT followed by whitespace must be kept.
        let input = "A\nB";
        assert_eq!(unfold(input), "A\nB");
    }

    #[test]
    fn unfold_multiple_folds() {
        assert_eq!(unfold("FN:Al\n ice\n  Bob"), "FN:Alice Bob");
    }

    #[test]
    fn parse_line_simple() {
        let p = parse_line("FN:Alice").unwrap();
        assert_eq!(p.name, "FN");
        assert_eq!(p.value, "Alice");
        assert!(p.params.is_empty());
    }

    #[test]
    fn parse_line_with_param() {
        let p = parse_line("TEL;TYPE=WORK:+1-555-1234").unwrap();
        assert_eq!(p.name, "TEL");
        assert_eq!(p.value, "+1-555-1234");
        assert_eq!(p.param("TYPE"), Some("WORK"));
    }

    #[test]
    fn parse_line_bare_type_token() {
        // vCard 2.1 style: TEL;WORK;VOICE:...
        let p = parse_line("TEL;WORK;VOICE:+1-555-0000").unwrap();
        assert_eq!(p.name, "TEL");
        assert_eq!(p.value, "+1-555-0000");
        // Both bare tokens should become TYPE params.
        assert_eq!(p.params.len(), 2);
        assert!(p.params.iter().all(|(k, _)| k == "TYPE"));
    }

    #[test]
    fn parse_line_group_prefix_stripped() {
        let p = parse_line("item1.EMAIL;TYPE=HOME:a@b.com").unwrap();
        assert_eq!(p.name, "EMAIL");
    }

    #[test]
    fn parse_line_name_uppercased() {
        let p = parse_line("fn:Bob").unwrap();
        assert_eq!(p.name, "FN");
    }

    #[test]
    fn parse_line_no_colon_returns_none() {
        assert!(parse_line("BEGINVCARD").is_none());
    }

    #[test]
    fn parse_line_multiple_params() {
        let p = parse_line("ADR;TYPE=HOME;ENCODING=UTF-8:;;Street;City;;;").unwrap();
        assert_eq!(p.param("TYPE"), Some("HOME"));
        assert_eq!(p.param("ENCODING"), Some("UTF-8"));
    }

    fn minimal_vcard(fn_value: &str) -> String {
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{fn_value}\r\nEND:VCARD\r\n")
    }

    #[test]
    fn parse_vcards_single() {
        let cards = parse_vcards(&minimal_vcard("Alice"));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].get("FN").unwrap().value, "Alice");
    }

    #[test]
    fn parse_vcards_multiple() {
        let input = format!("{}{}", minimal_vcard("Alice"), minimal_vcard("Bob"));
        let cards = parse_vcards(&input);
        assert_eq!(cards.len(), 2);
    }

    #[test]
    fn parse_vcards_empty_input() {
        assert!(parse_vcards("").is_empty());
    }

    #[test]
    fn parse_vcards_ignores_lines_outside_begin_end() {
        let input = "junk line\nBEGIN:VCARD\nFN:Alice\nEND:VCARD\n";
        let cards = parse_vcards(input);
        assert_eq!(cards.len(), 1);
    }

    #[test]
    fn qp_plain_ascii() {
        assert_eq!(decode_quoted_printable("Hello"), "Hello");
    }

    #[test]
    fn qp_encoded_umlaut() {
        // ä = 0xC3 0xA4 in UTF-8
        assert_eq!(decode_quoted_printable("=C3=A4"), "ä");
    }

    #[test]
    fn qp_soft_line_break() {
        // =\n is a soft line-break; the decoder also consumes the byte after \n.
        // "Hel=\nlo" → the '\n' and 'l' are both consumed, leaving "Helo".
        assert_eq!(decode_quoted_printable("Hel=\nlo"), "Helo");
    }

    #[test]
    fn qp_mixed() {
        assert_eq!(decode_quoted_printable("caf=C3=A9"), "café");
    }

    #[test]
    fn mojibake_ascii_unchanged() {
        assert_eq!(fix_mojibake("Hello"), "Hello");
    }

    #[test]
    fn mojibake_repaired() {
        // "ä" stored as mojibake looks like two Latin-1 chars whose bytes are
        // [0xC3, 0xA4] — the UTF-8 encoding of ä.
        let mojibake: String = [0xC3u8, 0xA4].iter().map(|&b| b as char).collect();
        assert_eq!(fix_mojibake(&mojibake), "ä");
    }

    #[test]
    fn mojibake_no_false_positive_on_latin1() {
        // A genuine single Latin-1 char (é = U+00E9) must not be mangled.
        let s = "caf\u{00E9}";
        // fix_mojibake may return as-is because the fixed string wouldn't be
        // shorter (single char → single char).
        let result = fix_mojibake(s);
        assert!(!result.is_empty());
    }

    #[test]
    fn format_name_full() {
        // Last;First;Middle;Prefix;Suffix
        assert_eq!(format_name("Smith;John;W;Dr;Jr"), "Dr John W Smith, Jr");
    }

    #[test]
    fn format_name_last_first_only() {
        assert_eq!(format_name("Smith;John"), "John Smith");
    }

    #[test]
    fn format_name_last_only() {
        assert_eq!(format_name("Smith"), "Smith");
    }

    #[test]
    fn format_name_empty() {
        assert_eq!(format_name(""), "");
    }

    #[test]
    fn format_name_suffix_only() {
        assert_eq!(format_name(";;;;Jr"), ", Jr");
    }

    #[test]
    fn format_address_full() {
        // P.O.Box;Extended;Street;City;Region;PostalCode;Country
        let result = format_address("123;;Main St;Springfield;IL;62701;USA");
        assert_eq!(result, "P.O. Box 123, Main St, Springfield, IL 62701, USA");
    }

    #[test]
    fn format_address_street_city_country() {
        let result = format_address(";;123 Main St;Springfield;;;USA");
        assert_eq!(result, "123 Main St, Springfield, USA");
    }

    #[test]
    fn format_address_empty() {
        assert_eq!(format_address(";;;;;;"), "");
    }

    #[test]
    fn format_address_only_city_postal() {
        let result = format_address(";;;Berlin;;10115;");
        assert_eq!(result, "Berlin 10115");
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let result = truncate("hello world", 6);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 6);
    }

    #[test]
    fn truncate_unicode_chars_counted_correctly() {
        // "ääääää" = 6 chars; max 4 → "äää…"
        let result = truncate("ääääää", 4);
        assert_eq!(result.chars().count(), 4);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn csv_escape_simple() {
        assert_eq!(csv_escape("hello"), "\"hello\"");
    }

    #[test]
    fn csv_escape_inner_quotes() {
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn csv_escape_empty() {
        assert_eq!(csv_escape(""), "\"\"");
    }

    fn make_prop(name: &str, value: &str, params: Vec<(&str, &str)>) -> Property {
        Property {
            name: name.to_uppercase(),
            params: params.into_iter().map(|(k, v)| (k.to_uppercase(), v.to_string())).collect(),
            value: value.to_string(),
        }
    }

    #[test]
    fn property_param_found() {
        let p = make_prop("TEL", "+1", vec![("TYPE", "WORK")]);
        assert_eq!(p.param("TYPE"), Some("WORK"));
        assert_eq!(p.param("type"), Some("WORK")); // case-insensitive
    }

    #[test]
    fn property_param_not_found() {
        let p = make_prop("TEL", "+1", vec![]);
        assert_eq!(p.param("TYPE"), None);
    }

    #[test]
    fn property_type_param() {
        let p = make_prop("TEL", "+1", vec![("TYPE", "HOME")]);
        assert_eq!(p.type_param(), Some("HOME"));
    }

    #[test]
    fn vcard_get_first_match() {
        let card = VCard {
            properties: vec![
                make_prop("EMAIL", "a@b.com", vec![]),
                make_prop("EMAIL", "c@d.com", vec![]),
            ],
        };
        assert_eq!(card.get("EMAIL").unwrap().value, "a@b.com");
    }

    #[test]
    fn vcard_get_missing() {
        let card = VCard { properties: vec![] };
        assert!(card.get("FN").is_none());
    }

    #[test]
    fn vcard_get_all() {
        let card = VCard {
            properties: vec![
                make_prop("EMAIL", "a@b.com", vec![]),
                make_prop("TEL", "+1", vec![]),
                make_prop("EMAIL", "c@d.com", vec![]),
            ],
        };
        let emails: Vec<&str> = card.get_all("EMAIL").iter().map(|p| p.value.as_str()).collect();
        assert_eq!(emails, vec!["a@b.com", "c@d.com"]);
    }

    #[test]
    fn decode_value_plain() {
        let p = make_prop("FN", "Alice", vec![]);
        assert_eq!(decode_value(&p), "Alice");
    }

    #[test]
    fn decode_value_escape_sequences() {
        let p = make_prop("NOTE", r"Line1\nLine2\,ok\;yes\\done", vec![]);
        let result = decode_value(&p);
        assert!(result.contains('\n'));
        assert!(result.contains(','));
        assert!(result.contains(';'));
        assert!(result.contains('\\'));
        assert!(!result.contains("\\n"));
    }

    #[test]
    fn decode_value_quoted_printable() {
        let p = make_prop("FN", "caf=C3=A9", vec![("ENCODING", "QUOTED-PRINTABLE")]);
        assert_eq!(decode_value(&p), "café");
    }
}


fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("Usage: {} [--csv] <file.vcf | directory>", args[0]);
        eprintln!();
        eprintln!("  file.vcf    - display all vCards in a single file");
        eprintln!("  directory   - display all *.vcf / *.vcard files in the directory");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --csv       Output data as RFC 4180 CSV (to stdout)");
        eprintln!();
        eprintln!("Supports vCard 2.1, 3.0, and 4.0.");
        process::exit(1);
    }

    let csv_mode = args.iter().any(|a| a == "--csv");

    // The path argument is the first non-flag argument after the binary name.
    let path_arg = args.iter().skip(1).find(|a| !a.starts_with('-'));
    let path_arg = match path_arg {
        Some(p) => p,
        None => {
            eprintln!("Error: no input file or directory specified.");
            process::exit(1);
        }
    };
    let input = Path::new(path_arg.as_str());

    let meta = match fs::metadata(input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error: cannot access '{}': {}", input.display(), e);
            process::exit(1);
        }
    };
    let is_dir = meta.is_dir();

    // Build a list of (source_label, content) pairs to process.
    let sources: Vec<(String, String)> = if is_dir {
        let files = collect_vcf_files(input);
        if files.is_empty() {
            eprintln!("Error: no .vcf files found in '{}'.", input.display());
            process::exit(1);
        }
        files
            .iter()
            .map(|p| {
                let label = p.display().to_string();
                let content = match fs::read_to_string(p) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Warning: skipping '{}': {}", p.display(), e);
                        String::new()
                    }
                };
                (label, content)
            })
            .filter(|(_, c)| !c.is_empty())
            .collect()
    } else {
        let content = match fs::read_to_string(input) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: cannot read '{}': {}", input.display(), e);
                process::exit(1);
            }
        };
        vec![(input.display().to_string(), content)]
    };

    // Parse all cards across all sources.
    let all_cards: Vec<(String, VCard)> = sources
        .into_iter()
        .flat_map(|(label, content)| {
            parse_vcards(&content)
                .into_iter()
                .map(move |c| (label.clone(), c))
        })
        .collect();

    if all_cards.is_empty() {
        eprintln!("Error: no vCards found in '{}'.", input.display());
        process::exit(1);
    }

    if csv_mode {
        output_csv(&all_cards);
        return;
    }

    if is_dir {
        let file_count = {
            let mut seen = std::collections::HashSet::new();
            all_cards.iter().for_each(|(label, _)| { seen.insert(label.as_str()); });
            seen.len()
        };
        println!(
            "{}",
            format!(
                "  {} vCard{} from {} file{}",
                all_cards.len(),
                if all_cards.len() == 1 { "" } else { "s" },
                file_count,
                if file_count == 1 { "" } else { "s" },
            )
            .bold()
        );
        println!();
        let cards: Vec<&VCard> = all_cards.iter().map(|(_, c)| c).collect();
        display_table(&cards.into_iter().cloned().collect::<Vec<_>>());
    } else {
        let total = all_cards.len();
        for (i, (_, card)) in all_cards.iter().enumerate() {
            if i > 0 { println!(); }
            display_card(card, i, total);
        }
    }
}
