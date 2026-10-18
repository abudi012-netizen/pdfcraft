//! Rich Text Format (.rtf, RTF 1.9.1): paragraphs, bold, italic, sizes, alignment, page breaks,
//! tables and PNG/JPEG pictures, in the document's code page with `\u` Unicode escapes. Font,
//! colour and style tables, headers, footers and fields' instructions are read past.

use super::{Align, Block, Cell, Flow, Para, Picture, Run, Table};

/// Destinations whose text is not part of the document.
const SKIPPED: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "listtable",
    "listoverridetable",
    "revtbl",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "mmathPr",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "footnote",
    "fldinst",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "pn",
    "object",
    "shpinst",
    "nonshppict",
    "bkmkstart",
    "bkmkend",
    "filetbl",
    "pgdsctbl",
    "operator",
    "author",
    "title",
    "company",
];

/// Most nested groups followed (deeper ones are read as text only).
const MAX_DEPTH: usize = 256;

#[derive(Clone)]
struct Group {
    bold: bool,
    italic: bool,
    /// Half-points.
    size: u32,
    skip: bool,
    /// Characters to skip after `\uN`.
    uc: u32,
    /// Reading a `\pict`: its hex data, kind and goal size in twips.
    pict: bool,
}

#[derive(Default)]
struct Pict {
    hex: Vec<u8>,
    kind: Option<&'static str>,
    goal: (Option<f64>, Option<f64>),
}

struct Reader {
    flow: Flow,
    para: Para,
    encoding: &'static encoding_rs::Encoding,
    /// Bytes in the code page waiting to be decoded together (a multi-byte character).
    pending: Vec<u8>,
    in_table: bool,
    table: Table,
    row: Vec<Cell>,
    cell: Vec<Para>,
    pict: Pict,
    skip_chars: u32,
}

impl Reader {
    fn decode_pending(&mut self, g: &Group) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let text = self.encoding.decode_without_bom_handling(&bytes).0.into_owned();
        self.push_text(&text, g);
    }

    fn push_text(&mut self, text: &str, g: &Group) {
        if g.skip || text.is_empty() {
            return;
        }
        let size = Some(f64::from(g.size) / 2.0).filter(|s| (1.0..=400.0).contains(s));
        match self.para.runs.last_mut() {
            Some(r) if r.bold == g.bold && r.italic == g.italic && r.size == size => r.text.push_str(text),
            _ => self.para.runs.push(Run { text: text.to_owned(), bold: g.bold, italic: g.italic, size }),
        }
    }

    fn end_para(&mut self) {
        let para = Para { align: self.para.align, ..Default::default() };
        let done = std::mem::replace(&mut self.para, para);
        if self.in_table {
            self.cell.push(done);
        } else {
            self.finish_table();
            self.flow.blocks.push(Block::Para(done));
        }
    }

    fn end_cell(&mut self) {
        let para = std::mem::take(&mut self.para);
        if !para.runs.is_empty() {
            self.cell.push(para);
        }
        self.row.push(Cell { paras: std::mem::take(&mut self.cell), span: 1 });
    }

    fn end_row(&mut self) {
        if !self.row.is_empty() {
            self.table.rows.push(std::mem::take(&mut self.row));
        }
    }

    fn finish_table(&mut self) {
        self.end_row();
        if !self.table.rows.is_empty() {
            self.flow.blocks.push(Block::Table(std::mem::take(&mut self.table)));
        }
    }

    fn picture(&mut self) {
        let pict = std::mem::take(&mut self.pict);
        let Some(_) = pict.kind else {
            self.flow.skipped = self.flow.skipped.saturating_add(1);
            return;
        };
        let digit = |b: u8| char::from(b).to_digit(16);
        let data: Vec<u8> = pict.hex.as_chunks::<2>().0.iter().filter_map(|[hi, lo]| Some((digit(*hi)? << 4 | digit(*lo)?) as u8)).collect();
        let size = match pict.goal {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w / 20.0, h / 20.0)),
            _ => None,
        };
        self.decode_pending(&Group { bold: false, italic: false, size: 24, skip: false, uc: 1, pict: false });
        let para = std::mem::take(&mut self.para);
        if !para.runs.is_empty() {
            self.flow.blocks.push(Block::Para(para));
        }
        self.flow.blocks.push(Block::Picture(Picture { data, size }));
    }
}

pub(crate) fn read(bytes: &[u8]) -> Flow {
    let mut r = Reader {
        flow: Flow::new(super::A4),
        para: Para::default(),
        encoding: encoding_rs::WINDOWS_1252,
        pending: Vec::new(),
        in_table: false,
        table: Table::default(),
        row: Vec::new(),
        cell: Vec::new(),
        pict: Pict::default(),
        skip_chars: 0,
    };
    r.flow.margins = [72.0, 90.0, 72.0, 90.0];
    let mut stack: Vec<Group> = Vec::new();
    let mut g = Group { bold: false, italic: false, size: 24, skip: false, uc: 1, pict: false };
    // Whether the group just opened may name a destination (`{\*\dest` or `{\fonttbl`).
    let mut group_start = false;
    let mut ignorable = false;
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        i += 1;
        match b {
            b'{' => {
                r.decode_pending(&g);
                if stack.len() < MAX_DEPTH {
                    stack.push(g.clone());
                }
                group_start = true;
                ignorable = false;
                continue;
            }
            b'}' => {
                r.decode_pending(&g);
                if g.pict && !stack.last().is_some_and(|p| p.pict) {
                    r.picture();
                }
                if let Some(prev) = stack.pop() {
                    g = prev;
                }
                group_start = false;
                continue;
            }
            b'\\' => {
                let Some(&next) = bytes.get(i) else { break };
                if next.is_ascii_alphabetic() {
                    let start = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) && i - start < 32 {
                        i += 1;
                    }
                    let word = std::str::from_utf8(bytes.get(start..i).unwrap_or_default()).unwrap_or("");
                    let num_start = i;
                    if bytes.get(i) == Some(&b'-') {
                        i += 1;
                    }
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) && i - num_start < 10 {
                        i += 1;
                    }
                    let param = std::str::from_utf8(bytes.get(num_start..i).unwrap_or_default()).ok().and_then(|n| n.parse::<i64>().ok());
                    if bytes.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    if r.skip_chars > 0 && !g.skip {
                        // A control word stands for one of the characters to skip after \u.
                        r.skip_chars -= 1;
                        continue;
                    }
                    if word != "u" {
                        r.decode_pending(&g);
                    }
                    if group_start && (ignorable || SKIPPED.contains(&word)) && word != "pict" && word != "shppict" {
                        g.skip = true;
                    }
                    group_start = false;
                    control(&mut r, &mut g, word, param, &mut i, bytes);
                } else {
                    i += 1;
                    match next {
                        b'\'' => {
                            let hex = bytes.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                            i += 2;
                            if let Some(byte) = hex {
                                if r.skip_chars > 0 {
                                    r.skip_chars -= 1;
                                } else if !g.skip {
                                    r.pending.push(byte);
                                }
                            }
                        }
                        b'*' => ignorable = true,
                        b'~' => r.push_text("\u{a0}", &g),
                        b'_' => r.push_text("-", &g),
                        b'-' => {}
                        b'\n' | b'\r' => {
                            if !g.skip {
                                r.decode_pending(&g);
                                r.end_para();
                            }
                        }
                        c => {
                            r.decode_pending(&g);
                            r.push_text(&char::from(c).to_string(), &g);
                        }
                    }
                }
                continue;
            }
            b'\r' | b'\n' => continue,
            _ => {}
        }
        group_start = false;
        if g.pict {
            if b.is_ascii_hexdigit() && r.pict.hex.len() < 64 << 20 {
                r.pict.hex.push(b);
            }
            continue;
        }
        if r.skip_chars > 0 {
            r.skip_chars -= 1;
            continue;
        }
        if !g.skip {
            r.pending.push(b);
        }
    }
    r.decode_pending(&g);
    if !r.para.runs.is_empty() {
        r.end_para();
    }
    if r.in_table {
        r.in_table = false;
        if !r.cell.is_empty() {
            r.end_cell();
        }
    }
    r.finish_table();
    r.flow
}

fn control(r: &mut Reader, g: &mut Group, word: &str, param: Option<i64>, i: &mut usize, bytes: &[u8]) {
    let on = param != Some(0);
    let twips = |p: Option<i64>| p.filter(|v| *v > 0 && *v < 1_000_000).map(|v| v as f64 / 20.0);
    match word {
        "ansicpg" => {
            if let Some(enc) = param.and_then(|cp| encoding_rs::Encoding::for_label(format!("windows-{cp}").as_bytes())) {
                r.encoding = enc;
            }
        }
        "mac" => r.encoding = encoding_rs::MACINTOSH,
        "paperw" => {
            if let Some(w) = twips(param) {
                r.flow.page.0 = w;
            }
        }
        "paperh" => {
            if let Some(h) = twips(param) {
                r.flow.page.1 = h;
            }
        }
        "margl" => r.flow.margins[3] = twips(param).unwrap_or(r.flow.margins[3]),
        "margr" => r.flow.margins[1] = twips(param).unwrap_or(r.flow.margins[1]),
        "margt" => r.flow.margins[0] = twips(param).unwrap_or(r.flow.margins[0]),
        "margb" => r.flow.margins[2] = twips(param).unwrap_or(r.flow.margins[2]),
        // A picture in a skipped group (`\nonshppict`, the copy for old readers) is not shown.
        "pict" if !g.skip => {
            g.pict = true;
            g.skip = true;
            r.pict = Pict::default();
        }
        "pngblip" => r.pict.kind = Some("png"),
        "jpegblip" => r.pict.kind = Some("jpeg"),
        "picwgoal" => r.pict.goal.0 = twips(param).map(|v| v * 20.0),
        "pichgoal" => r.pict.goal.1 = twips(param).map(|v| v * 20.0),
        "bin" => {
            // Raw picture bytes: kept as hex so the picture reader treats them alike.
            let n = usize::try_from(param.unwrap_or(0)).unwrap_or(0);
            let data = bytes.get(*i..i.saturating_add(n)).unwrap_or_default();
            if g.pict {
                for b in data {
                    r.pict.hex.extend_from_slice(format!("{b:02x}").as_bytes());
                }
            }
            *i = i.saturating_add(n);
        }
        _ if g.skip => {}
        "par" | "sect" => r.end_para(),
        "line" => r.push_text("\n", g),
        "tab" => r.push_text("\t", g),
        "page" => {
            r.end_para();
            r.flow.blocks.push(Block::PageBreak);
        }
        "pard" => {
            r.para.align = Align::Left;
            r.in_table = false;
        }
        "plain" => {
            g.bold = false;
            g.italic = false;
            g.size = 24;
        }
        "b" => g.bold = on,
        "i" => g.italic = on,
        "fs" => g.size = param.and_then(|v| u32::try_from(v).ok()).filter(|v| (2..=800).contains(v)).unwrap_or(24),
        "qc" => r.para.align = Align::Center,
        "qr" => r.para.align = Align::Right,
        "ql" | "qj" => r.para.align = Align::Left,
        "uc" => g.uc = param.and_then(|v| u32::try_from(v).ok()).unwrap_or(1).min(8),
        "u" => {
            if let Some(v) = param {
                r.decode_pending(g);
                let code = if v < 0 { v + 65_536 } else { v };
                if let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) {
                    r.push_text(&c.to_string(), g);
                }
                r.skip_chars = g.uc;
            }
        }
        "intbl" => r.in_table = true,
        "trowd" => {
            r.in_table = true;
        }
        "cell" => {
            r.in_table = true;
            r.end_cell();
        }
        "row" => {
            r.end_row();
            r.in_table = false;
        }
        "emdash" => r.push_text("—", g),
        "endash" => r.push_text("–", g),
        "bullet" => r.push_text("•", g),
        "lquote" => r.push_text("‘", g),
        "rquote" => r.push_text("’", g),
        "ldblquote" => r.push_text("“", g),
        "rdblquote" => r.push_text("”", g),
        "emspace" | "enspace" | "qmspace" => r.push_text(" ", g),
        _ => {}
    }
}
