//! Web pages (.html): headings, paragraphs, bold and italic, lists, tables, line breaks and
//! pictures carried in the page (`data:` URLs). Nothing is fetched: linked pictures, scripts and
//! style sheets are left out. A forgiving reader, as browsers are: unclosed tags close
//! themselves.

use super::{Align, Block, Cell, Flow, Para, Picture, Run, Table};

/// An HTML character reference's text (`&amp;`, `&#233;`, `&#x2014;`); `None` if unknown.
fn entity(name: &str) -> Option<String> {
    if let Some(n) = name.strip_prefix('#') {
        let code = match n.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => n.parse::<u32>().ok()?,
        };
        return char::from_u32(code).filter(|c| *c != '\0').map(String::from);
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "micro" => 'µ',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "harr" => '↔',
        "le" => '≤',
        "ge" => '≥',
        "ne" => '≠',
        "asymp" => '≈',
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "Delta" => 'Δ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "pi" => 'π',
        "sigma" => 'σ',
        "Sigma" => 'Σ',
        "omega" => 'ω',
        "Omega" => 'Ω',
        "sup2" => '²',
        "sup3" => '³',
        "frac12" => '½',
        "frac14" => '¼',
        _ => return None,
    };
    Some(c.to_string())
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or_default());
        let after = rest.get(at + 1..).unwrap_or_default();
        let end = after.find(';').filter(|&e| e <= 32);
        match end.and_then(|e| entity(after.get(..e)?).map(|t| (e, t))) {
            Some((e, text)) => {
                out.push_str(&text);
                rest = after.get(e + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A start or end tag: its lower-case name and its attributes.
struct Tag {
    name: String,
    end: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn centered(&self) -> bool {
        self.attr("align").is_some_and(|a| a.eq_ignore_ascii_case("center"))
            || self.attr("style").is_some_and(|s| s.replace(' ', "").to_ascii_lowercase().contains("text-align:center"))
    }
}

fn parse_tag(inner: &str) -> Tag {
    let inner = inner.trim();
    let (end, inner) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name_end = inner.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(inner.len());
    let name = inner.get(..name_end).unwrap_or_default().to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut rest = inner.get(name_end..).unwrap_or_default();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() || attrs.len() > 64 {
            break;
        }
        let key_end = rest.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(rest.len());
        let key = rest.get(..key_end).unwrap_or_default().to_ascii_lowercase();
        rest = rest.get(key_end..).unwrap_or_default().trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            if let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let body = after.get(1..).unwrap_or_default();
                let close = body.find(q).unwrap_or(body.len());
                value = body.get(..close).unwrap_or_default().to_owned();
                rest = body.get(close + 1..).unwrap_or_default();
            } else {
                let close = after.find(char::is_whitespace).unwrap_or(after.len());
                value = after.get(..close).unwrap_or_default().to_owned();
                rest = after.get(close..).unwrap_or_default();
            }
        }
        if key.is_empty() {
            // Not an attribute: skip a character so the loop moves on.
            rest = rest.get(rest.chars().next().map_or(0, char::len_utf8)..).unwrap_or_default();
            continue;
        }
        attrs.push((key, decode_entities(&value)));
    }
    Tag { name, end, attrs }
}

/// Base64 (standard alphabet, padding and whitespace ignored).
fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A picture in a `data:image/...;base64,` URL.
fn data_picture(src: &str) -> Option<Vec<u8>> {
    let rest = src.trim().strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    (meta.to_ascii_lowercase().starts_with("image/") && meta.ends_with(";base64")).then(|| base64(data)).flatten()
}

fn css_px(v: Option<&str>) -> Option<f64> {
    let v = v?.trim().trim_end_matches("px");
    v.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0).map(|px| px * 0.75)
}

#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    para: Para,
    bold: u32,
    italic: u32,
    pre: u32,
    /// Open lists: numbered, and the next number.
    lists: Vec<(bool, u32)>,
    /// The table being read, its row, and the cell's paragraphs.
    table: Option<(Table, Vec<Cell>, Option<Vec<Para>>)>,
    nested_tables: u32,
    /// The open cell's column span.
    pending_span: usize,
    skipped: usize,
}

impl State {
    fn text(&mut self, raw: &str) {
        let text = decode_entities(raw);
        let text = if self.pre > 0 {
            text
        } else {
            let mut out = String::with_capacity(text.len());
            let mut space = self.para.runs.last().is_none_or(|r| r.text.ends_with([' ', '\n']));
            for c in text.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !space {
                        out.push(' ');
                        space = true;
                    }
                } else {
                    out.push(c);
                    space = false;
                }
            }
            out
        };
        if text.is_empty() {
            return;
        }
        let (bold, italic) = (self.bold > 0, self.italic > 0);
        match self.para.runs.last_mut() {
            Some(r) if r.bold == bold && r.italic == italic => r.text.push_str(&text),
            _ => self.para.runs.push(Run { text, bold, italic, size: None }),
        }
    }

    /// End the paragraph being read (if it has anything) and start one like `next`.
    fn flush(&mut self, next: Para) {
        let para = std::mem::replace(&mut self.para, next);
        if para.runs.iter().all(|r| r.text.trim().is_empty()) && para.bullet.is_none() {
            return;
        }
        match &mut self.table {
            Some((_, _, Some(cell))) => cell.push(para),
            _ => self.blocks.push(Block::Para(para)),
        }
    }

    fn tag(&mut self, tag: &Tag) {
        let heading = tag.name.strip_prefix('h').and_then(|l| l.parse::<u8>().ok()).filter(|l| (1..=6).contains(l));
        match (tag.name.as_str(), tag.end) {
            ("b" | "strong", false) => self.bold = self.bold.saturating_add(1),
            ("b" | "strong", true) => self.bold = self.bold.saturating_sub(1),
            ("i" | "em" | "cite" | "var", false) => self.italic = self.italic.saturating_add(1),
            ("i" | "em" | "cite" | "var", true) => self.italic = self.italic.saturating_sub(1),
            ("pre", false) => {
                self.flush(Para::default());
                self.pre = self.pre.saturating_add(1);
            }
            ("pre", true) => {
                self.flush(Para::default());
                self.pre = self.pre.saturating_sub(1);
            }
            ("br", _) => self.para.runs.push(Run { text: "\n".into(), bold: self.bold > 0, italic: self.italic > 0, size: None }),
            ("p" | "div" | "section" | "article" | "header" | "footer" | "blockquote" | "dd" | "dt" | "figcaption" | "address" | "center", false) => {
                let center = tag.centered() || tag.name == "center";
                self.flush(Para { align: if center { Align::Center } else { Align::Left }, ..Default::default() });
            }
            ("p" | "div" | "section" | "article" | "header" | "footer" | "blockquote" | "dd" | "dt" | "figcaption" | "address" | "center", true) => {
                self.flush(Para::default())
            }
            (_, false) if heading.is_some() => {
                self.flush(Para { heading, align: if tag.centered() { Align::Center } else { Align::Left }, ..Default::default() })
            }
            (_, true) if heading.is_some() => self.flush(Para::default()),
            ("hr", _) => self.flush(Para::default()),
            ("ul" | "ol", false) => {
                self.flush(Para::default());
                let start = tag.attr("start").and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
                if self.lists.len() < 16 {
                    self.lists.push((tag.name == "ol", start));
                }
            }
            ("ul" | "ol", true) => {
                self.flush(Para::default());
                self.lists.pop();
            }
            ("li", false) => {
                let level = self.lists.len().saturating_sub(1).min(8) as u8;
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        let m = format!("{n}.");
                        *n = n.saturating_add(1);
                        m
                    }
                    _ => if level == 0 { "•" } else { "–" }.to_owned(),
                };
                self.flush(Para { bullet: Some(marker), level, ..Default::default() });
            }
            ("li", true) => self.flush(Para::default()),
            ("table", false) => {
                self.flush(Para::default());
                if self.table.is_some() {
                    self.nested_tables = self.nested_tables.saturating_add(1);
                } else {
                    self.table = Some((Table::default(), Vec::new(), None));
                }
            }
            ("table", true) => {
                if self.nested_tables > 0 {
                    self.nested_tables -= 1;
                    return;
                }
                self.end_cell();
                self.end_row();
                if let Some((table, _, _)) = self.table.take()
                    && !table.rows.is_empty()
                {
                    self.blocks.push(Block::Table(table));
                }
            }
            ("tr", false) if self.nested_tables == 0 => {
                self.end_cell();
                self.end_row();
            }
            ("tr", true) if self.nested_tables == 0 => {
                self.end_cell();
                self.end_row();
            }
            ("td" | "th", false) if self.nested_tables == 0 => {
                self.end_cell();
                if let Some((_, _, cell)) = &mut self.table {
                    *cell = Some(Vec::new());
                }
                if tag.name == "th" {
                    self.bold = self.bold.saturating_add(1);
                }
                let span = tag.attr("colspan").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                self.para = Para::default();
                self.pending_span = span;
            }
            ("td" | "th", true) if self.nested_tables == 0 => {
                self.end_cell();
                if tag.name == "th" {
                    self.bold = self.bold.saturating_sub(1);
                }
            }
            ("td" | "th" | "tr", _) => self.text(" "),
            ("img", _) => match tag.attr("src").and_then(data_picture) {
                Some(data) => {
                    let size = css_px(tag.attr("width")).zip(css_px(tag.attr("height")));
                    self.flush(Para::default());
                    match &mut self.table {
                        Some(_) => self.skipped = self.skipped.saturating_add(1),
                        None => self.blocks.push(Block::Picture(Picture { data, size })),
                    }
                }
                None => {
                    if let Some(alt) = tag.attr("alt").filter(|a| !a.trim().is_empty()) {
                        self.text(&format!("[{alt}]"));
                    }
                    self.skipped = self.skipped.saturating_add(1);
                }
            },
            _ => {}
        }
    }
}

impl State {
    fn end_cell(&mut self) {
        let span = std::mem::replace(&mut self.pending_span, 1);
        let para = std::mem::take(&mut self.para);
        if let Some((_, row, cell @ Some(_))) = &mut self.table {
            let mut paras = cell.take().unwrap_or_default();
            if !para.runs.iter().all(|r| r.text.trim().is_empty()) {
                paras.push(para);
            }
            row.push(Cell { paras, span });
        } else {
            self.para = para;
        }
    }

    fn end_row(&mut self) {
        if let Some((table, row, _)) = &mut self.table
            && !row.is_empty()
        {
            table.rows.push(std::mem::take(row));
        }
    }
}

pub(crate) fn read(html: &str) -> Flow {
    let mut s = State { pending_span: 1, ..Default::default() };
    let mut rest = html;
    // Text inside these is not shown.
    let mut skip_until: Option<&str> = None;
    while !rest.is_empty() {
        if let Some(until) = skip_until.take() {
            // Raw text (a script's `if (a < b)`): only its own end tag ends it.
            let lower = rest.to_ascii_lowercase();
            rest = lower
                .find(&format!("</{until}"))
                .and_then(|at| rest.get(at..))
                .and_then(|r| r.find('>').and_then(|e| r.get(e + 1..)))
                .unwrap_or_default();
            continue;
        }
        let Some(lt) = rest.find('<') else {
            if skip_until.is_none() {
                s.text(rest);
            }
            break;
        };
        if skip_until.is_none() {
            s.text(rest.get(..lt).unwrap_or_default());
        }
        rest = rest.get(lt..).unwrap_or_default();
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").and_then(|e| after.get(e + 3..)).unwrap_or_default();
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').and_then(|e| rest.get(e + 1..)).unwrap_or_default();
            continue;
        }
        // The tag's end: the first '>' outside quotes.
        let mut quote = None;
        let mut end = None;
        for (i, c) in rest.char_indices().skip(1) {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            if skip_until.is_none() {
                s.text(rest);
            }
            break;
        };
        let inner = rest.get(1..end).unwrap_or_default();
        rest = rest.get(end + 1..).unwrap_or_default();
        if !inner.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/') {
            if skip_until.is_none() {
                s.text("<");
                s.text(inner);
                s.text(">");
            }
            continue;
        }
        let tag = parse_tag(inner);
        if let Some(until) = skip_until {
            if tag.end && tag.name == until {
                skip_until = None;
            }
            continue;
        }
        match tag.name.as_str() {
            "script" | "style" | "title" | "noscript" | "template" | "svg" | "math" | "select" if !tag.end && !inner.trim_end().ends_with('/') => {
                skip_until = Some(match tag.name.as_str() {
                    "script" => "script",
                    "style" => "style",
                    "title" => "title",
                    "noscript" => "noscript",
                    "template" => "template",
                    "svg" => "svg",
                    "math" => "math",
                    _ => "select",
                });
                if tag.name == "svg" || tag.name == "math" {
                    s.skipped = s.skipped.saturating_add(1);
                }
            }
            _ => s.tag(&tag),
        }
    }
    s.flush(Para::default());
    if s.table.is_some() {
        s.tag(&Tag { name: "table".into(), end: true, attrs: Vec::new() });
    }
    let mut flow = Flow::new(super::A4);
    flow.margins = [56.0; 4];
    flow.blocks = s.blocks;
    flow.skipped = s.skipped;
    flow
}
