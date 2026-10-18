//! Create ▸ from Word (.docx), PowerPoint (.pptx), Rich Text (.rtf) and web pages (.html):
//! the reverse of Export. Each reader turns its file into paragraphs, pictures and tables (a
//! flowing document) or into slides of placed boxes; this module lays those out on PDF pages in
//! Inter, embedded whole so Greek, Cyrillic and symbols draw. The look is simplified: styles
//! become bold, italic, sizes, headings, bullets and alignment; colours, fonts and drawn shapes
//! are not kept.

use std::collections::HashMap;

use pdfcraft_cos::{Dict, Document, Object};
use pdfcraft_fonts::pdf::Metrics;

use crate::{CreateError, add_page, image_xobject, set_title};

mod docx;
mod html;
mod pptx;
mod rtf;
mod zip;

/// A4 in points: the page for web pages, and for RTF files that don't give one.
const A4: (f64, f64) = (595.0, 842.0);
/// Most pages one conversion makes.
const MAX_PAGES: usize = 10_000;
/// Most characters of text laid out (a very large document is cut, and the report says so).
const MAX_TEXT: usize = 20_000_000;

static INTER_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf");

// ── the model readers produce ───────────────────────────────────────────────────────────────

/// A piece of text in one style. `size` is in points; `None` takes the paragraph's.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub size: Option<f64>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self {
        Run { text: text.into(), ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Para {
    pub runs: Vec<Run>,
    /// 1 (largest) to 6.
    pub heading: Option<u8>,
    /// The list marker ("•", "3.", "b)").
    pub bullet: Option<String>,
    /// List or outline level, from 0.
    pub level: u8,
    pub align: Align,
}

impl Para {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.trim().is_empty()) && self.bullet.is_none()
    }
}

/// An image file's bytes and, when the document says, its size on the page in points.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Picture {
    pub data: Vec<u8>,
    pub size: Option<(f64, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cell {
    pub paras: Vec<Para>,
    /// Grid columns the cell spans (at least 1).
    pub span: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Table {
    /// Column widths, in any unit (only their proportions are used); empty for equal columns.
    pub widths: Vec<f64>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Para(Para),
    Picture(Picture),
    Table(Table),
    PageBreak,
}

/// A document whose text flows from page to page.
pub(crate) struct Flow {
    pub page: (f64, f64),
    /// Top, right, bottom, left, in points.
    pub margins: [f64; 4],
    pub blocks: Vec<Block>,
    /// Pictures and other content the reader could not keep.
    pub skipped: usize,
}

impl Flow {
    fn new(page: (f64, f64)) -> Self {
        Flow { page, margins: [72.0; 4], blocks: Vec::new(), skipped: 0 }
    }
}

/// A box on a slide: x, y from the top-left corner, width and height, in points.
pub(crate) type Rect = [f64; 4];

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    Text { rect: Rect, paras: Vec<Para>, size: f64 },
    Picture { rect: Rect, picture: Picture },
    Table { rect: Rect, table: Table },
}

/// Slides: one page each, every item placed where it is on the slide.
pub(crate) struct Deck {
    pub size: (f64, f64),
    pub slides: Vec<Vec<Item>>,
    pub skipped: usize,
}

/// A list bullet as drawn: Office writes bullets as letters in a symbol font ("§" in Wingdings
/// is a small square, "Ø" an arrowhead), or as those fonts' private-use codes. Those become
/// the shape they show; anything unknown in a symbol font becomes "•".
pub(crate) fn bullet_char(c: char, font: Option<&str>) -> String {
    let font = font.unwrap_or("").to_ascii_lowercase();
    let symbol_font = ["wingdings", "symbol", "webdings", "marlett", "zapf"].iter().any(|f| font.contains(f));
    let private = ('\u{f000}'..='\u{f0ff}').contains(&c);
    if !symbol_font && !private {
        return if ('\u{e000}'..='\u{f8ff}').contains(&c) { "•".into() } else { c.to_string() };
    }
    let code = if private { u32::from(c) - 0xF000 } else { u32::from(c) };
    let shape = if font.contains("symbol") {
        match code {
            0xB7 => '•',
            0xAE => '→',
            0x6F => '○',
            _ => '•',
        }
    } else {
        match code {
            0xD8 | 0xE0 | 0xE8 | 0xDE | 0xF0 | 0xC4 => '►',
            0xA7 => '▪',
            0x6E => '■',
            0x6C | 0x9F => '●',
            0xFC | 0xFE => '✓',
            0x76 => '❖',
            0x75 | 0x77 => '◆',
            0x71 | 0x6F | 0xA8 => '□',
            0xAB => '★',
            _ => '•',
        }
    };
    shape.to_string()
}

// ── what the caller gets ─────────────────────────────────────────────────────────────────────

/// File extensions [`from_office`] converts.
pub const OFFICE: [&str; 5] = ["docx", "pptx", "rtf", "html", "htm"];

/// What a conversion could not keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OfficeReport {
    /// Characters the embedded font has no glyph for (Arabic, Chinese…), drawn as "?".
    pub replaced_chars: usize,
    /// Pictures, charts and diagrams left out (a format PdfCraft can't draw, like EMF).
    pub skipped: usize,
    /// Text past the size limit was left out.
    pub truncated: bool,
}

impl OfficeReport {
    /// A sentence for the user, or `None` when everything was kept.
    pub fn message(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.skipped > 0 {
            parts.push(format!("{} picture(s) or chart(s) could not be converted and were left out", self.skipped));
        }
        if self.replaced_chars > 0 {
            parts.push(format!("{} character(s) the font can't draw are shown as \"?\"", self.replaced_chars));
        }
        if self.truncated {
            parts.push("the document was too long; its end was left out".to_owned());
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Docx,
    Pptx,
    Rtf,
    Html,
}

fn format(name: &str, bytes: &[u8]) -> Option<Format> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    match ext.as_str() {
        "docx" | "pptx" if bytes.starts_with(b"PK\x03\x04") => Some(if ext == "docx" { Format::Docx } else { Format::Pptx }),
        "rtf" if head.trim_ascii_start().starts_with(b"{\\rtf") => Some(Format::Rtf),
        "html" | "htm" => Some(Format::Html),
        _ => None,
    }
}

/// Whether `name`/`bytes` is a file [`from_office`] converts.
pub fn is_office(name: &str, bytes: &[u8]) -> bool {
    format(name, bytes).is_some()
}

/// Convert a Word, PowerPoint, RTF or HTML file to a PDF document.
pub fn from_office(name: &str, bytes: &[u8]) -> Result<(Document, OfficeReport), CreateError> {
    let fmt = format(name, bytes).ok_or_else(|| CreateError::Invalid(format!("{name}: not a Word, PowerPoint, RTF or HTML file")))?;
    let bad = |e: String| CreateError::Invalid(format!("{name}: {e}"));
    let mut w = Writer::new()?;
    match fmt {
        Format::Docx => w.flow(docx::read(bytes).map_err(bad)?)?,
        Format::Pptx => w.deck(pptx::read(bytes).map_err(bad)?)?,
        Format::Rtf => w.flow(rtf::read(bytes))?,
        Format::Html => w.flow(html::read(&crate::decode_text(bytes)))?,
    }
    let title = name.rsplit(['/', '\\']).next().unwrap_or(name);
    w.finish(title.rsplit_once('.').map_or(title, |(s, _)| s))
}

/// Parse an Office XML part, with a node budget (a part is at most a few hundred thousand).
pub(crate) fn xml(text: &str) -> Result<roxmltree::Document<'_>, String> {
    roxmltree::Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 4_000_000 })
        .map_err(|e| format!("damaged XML ({e})"))
}

// ── layout ───────────────────────────────────────────────────────────────────────────────────

struct Face {
    resource: String,
    metrics: Metrics,
    /// Each character's codes and advance (per point of size); `None` when the font lacks it.
    glyphs: HashMap<char, Option<(Vec<u8>, f64)>>,
}

impl Face {
    fn embed(doc: &mut Document, fonts: &mut Dict, bytes: &[u8], family: &str) -> Result<Self, CreateError> {
        let resource = pdfcraft_fonts::embedding::embed(doc, fonts, bytes, family).map_err(CreateError::Invalid)?;
        let dict =
            fonts.get(resource.as_bytes()).and_then(|o| doc.dict(o)).ok_or_else(|| CreateError::Invalid("the font was not embedded".into()))?;
        Ok(Face { metrics: Metrics::from_dict(doc, &dict), resource, glyphs: HashMap::new() })
    }

    fn glyph(&mut self, c: char) -> Option<(Vec<u8>, f64)> {
        let metrics = &self.metrics;
        self.glyphs
            .entry(c)
            .or_insert_with(|| {
                let codes = metrics.encode(c.encode_utf8(&mut [0; 4]))?;
                let width = metrics.codes(&codes).into_iter().map(|(code, _)| metrics.width(code)).sum::<f64>();
                width.is_finite().then_some((codes, width))
            })
            .clone()
    }
}

/// A run of one style on a line.
#[derive(Clone, Debug)]
struct Seg {
    text: String,
    bold: bool,
    italic: bool,
    size: f64,
    width: f64,
}

#[derive(Clone, Debug, Default)]
struct Line {
    segs: Vec<Seg>,
    width: f64,
    /// The largest size on the line.
    size: f64,
}

impl Line {
    fn height(&self) -> f64 {
        self.size * 1.3
    }
}

/// A paragraph cut into lines for a width.
struct Laid {
    lines: Vec<Line>,
    /// The marker and its size, drawn left of the first line.
    bullet: Option<(String, f64)>,
    indent: f64,
    align: Align,
    before: f64,
    after: f64,
}

impl Laid {
    fn height(&self) -> f64 {
        self.before + self.lines.iter().map(Line::height).sum::<f64>() + self.after
    }
}

struct Page {
    size: (f64, f64),
    content: Vec<u8>,
    images: Dict,
}

struct Writer {
    doc: Document,
    fonts: Dict,
    regular: Face,
    bold: Face,
    pages: Vec<Page>,
    report: OfficeReport,
    text_budget: usize,
    image_count: usize,
}

/// The size a heading is set in.
fn heading_size(level: u8) -> f64 {
    match level {
        1 => 20.0,
        2 => 16.0,
        3 => 14.0,
        _ => 12.0,
    }
}

/// `text` without the citation markers AI chat answers leave in copied text
/// (`[span_3](start_span)`, `[span_3](end_span)`): they mean nothing to a reader.
fn strip_citation_markers(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("_span)") {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("[span_") {
        out.push_str(rest.get(..at).unwrap_or_default());
        let after = rest.get(at + "[span_".len()..).unwrap_or_default();
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let tail = after.get(digits..).unwrap_or_default();
        let marker = ["](start_span)", "](end_span)"].iter().find(|m| digits > 0 && tail.starts_with(**m));
        match marker {
            Some(m) => rest = tail.get(m.len()..).unwrap_or_default(),
            None => {
                out.push_str("[span_");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.into()
}

/// Text as it is drawn: tabs as spaces, no control or invisible formatting characters, no AI
/// citation markers.
fn clean(text: &str) -> String {
    strip_citation_markers(text)
        .chars()
        .filter_map(|c| match c {
            '\t' => Some(' '),
            '\n' => Some('\n'),
            '\u{a0}' | '\u{2007}' | '\u{202f}' => Some(' '),
            '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn hex(codes: &[u8]) -> String {
    let mut s = String::with_capacity(codes.len() * 2 + 2);
    s.push('<');
    for b in codes {
        s.push_str(&format!("{b:02X}"));
    }
    s.push('>');
    s
}

impl Writer {
    fn new() -> Result<Self, CreateError> {
        let mut doc = Document::new_empty();
        let mut fonts = Dict::new();
        let regular = Face::embed(&mut doc, &mut fonts, pdfcraft_fonts::INTER_REGULAR, "Inter")?;
        let bold = Face::embed(&mut doc, &mut fonts, INTER_SEMIBOLD, "Inter-SemiBold")?;
        Ok(Writer { doc, fonts, regular, bold, pages: Vec::new(), report: OfficeReport::default(), text_budget: MAX_TEXT, image_count: 0 })
    }

    fn face(&mut self, bold: bool) -> &mut Face {
        if bold { &mut self.bold } else { &mut self.regular }
    }

    /// `c`'s codes and advance at `size`; a character the font lacks is drawn as "?".
    fn glyph(&mut self, bold: bool, c: char) -> (Vec<u8>, f64) {
        if let Some(g) = self.face(bold).glyph(c) {
            return g;
        }
        self.report.replaced_chars = self.report.replaced_chars.saturating_add(1);
        self.face(bold).glyph('?').unwrap_or((vec![0, 0], 0.5))
    }

    fn measure(&mut self, text: &str, bold: bool, size: f64) -> f64 {
        text.chars().map(|c| self.face(bold).glyph(c).map_or(0.6, |g| g.1)).sum::<f64>() * size
    }

    /// Cut `para` into lines `width` wide, `base` being the size of runs that give none, and
    /// `scale` shrinking every size (a slide's text box that overflows).
    fn lay(&mut self, para: &Para, width: f64, base: f64, scale: f64) -> Laid {
        let default = para.heading.map_or(base, heading_size);
        let mut lines = vec![Line::default()];
        let indent =
            if para.bullet.is_some() || para.level > 0 { 18.0 * (f64::from(para.level) + f64::from(u8::from(para.bullet.is_some()))) } else { 0.0 };
        let avail = (width - indent).max(24.0);
        let mut first_size = default * scale;
        for (i, run) in para.runs.iter().enumerate() {
            let size = (run.size.unwrap_or(default) * scale).clamp(3.0, 400.0);
            if i == 0 {
                first_size = size;
            }
            let bold = run.bold || para.heading.is_some();
            let mut text = clean(&run.text);
            if self.text_budget < text.len() {
                text = text.chars().take(self.text_budget).collect();
                self.report.truncated = true;
            }
            self.text_budget = self.text_budget.saturating_sub(text.len());
            // Words with the spaces after them; a line break is its own piece.
            let mut pieces: Vec<String> = Vec::new();
            let mut cur = String::new();
            for c in text.chars() {
                if c == '\n' {
                    if !cur.is_empty() {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    pieces.push("\n".into());
                } else {
                    if c != ' ' && cur.ends_with(' ') {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    cur.push(c);
                }
            }
            if !cur.is_empty() {
                pieces.push(cur);
            }
            for piece in pieces {
                if piece == "\n" {
                    let size = lines.last().map_or(size, |l| l.size);
                    lines.push(Line { size, ..Default::default() });
                    continue;
                }
                let w = self.measure(&piece, bold, size);
                let fits = |line: &Line, w: f64, piece: &str, this: &mut Writer| {
                    line.segs.is_empty() || line.width + w - this.measure(&piece[piece.trim_end().len()..], bold, size) <= avail
                };
                let line_fits = lines.last().is_some_and(|l| fits(l, w, &piece, self));
                if !line_fits {
                    lines.push(Line::default());
                }
                if w > avail && lines.last().is_some_and(|l| l.segs.is_empty()) {
                    // A word wider than the line: break it between characters.
                    let mut part = String::new();
                    let mut pw = 0.0;
                    for c in piece.chars() {
                        let cw = self.measure(c.encode_utf8(&mut [0; 4]), bold, size);
                        if pw + cw > avail && !part.is_empty() {
                            self.push(&mut lines, std::mem::take(&mut part), run, bold, size, pw);
                            lines.push(Line::default());
                            pw = 0.0;
                        }
                        part.push(c);
                        pw += cw;
                    }
                    if !part.is_empty() {
                        self.push(&mut lines, part, run, bold, size, pw);
                    }
                } else {
                    self.push(&mut lines, piece, run, bold, size, w);
                }
            }
        }
        for l in &mut lines {
            if l.size <= 0.0 {
                l.size = first_size;
            }
        }
        let before = match para.heading {
            Some(1 | 2) => default * scale * 0.8,
            Some(_) => default * scale * 0.5,
            None => 0.0,
        };
        Laid { lines, bullet: para.bullet.clone().map(|b| (b, first_size)), indent, align: para.align, before, after: (base * scale * 0.45).max(2.0) }
    }

    fn push(&mut self, lines: &mut Vec<Line>, text: String, run: &Run, bold: bool, size: f64, width: f64) {
        if lines.is_empty() {
            lines.push(Line::default());
        }
        if let Some(line) = lines.last_mut() {
            line.size = line.size.max(size);
            line.width += width;
            line.segs.push(Seg { text, bold, italic: run.italic, size, width });
        }
    }

    fn page(&mut self, size: (f64, f64)) -> Result<(), CreateError> {
        if self.pages.len() >= MAX_PAGES {
            return Err(CreateError::Invalid(format!("the document would be more than {MAX_PAGES} pages")));
        }
        self.pages.push(Page { size, content: Vec::new(), images: Dict::new() });
        Ok(())
    }

    fn text_at(&mut self, s: &str, bold: bool, italic: bool, size: f64, x: f64, y: f64) {
        let mut codes = Vec::new();
        for c in s.trim_end().chars() {
            codes.extend(self.glyph(bold, c).0);
        }
        if codes.is_empty() {
            return;
        }
        let font = if bold { self.bold.resource.clone() } else { self.regular.resource.clone() };
        let skew = if italic { 0.21 } else { 0.0 };
        let op = format!("BT /{font} {size:.2} Tf 1 0 {skew} 1 {x:.2} {y:.2} Tm {} Tj ET\n", hex(&codes));
        if let Some(p) = self.pages.last_mut() {
            p.content.extend_from_slice(op.as_bytes());
        }
    }

    /// Draw the laid paragraph's line `line` with its top at `top`, inside `x .. x + width`.
    fn draw_line(&mut self, laid: &Laid, index: usize, x: f64, top: f64, width: f64) {
        let Some(line) = laid.lines.get(index) else { return };
        let baseline = top - line.size;
        if index == 0
            && let Some((marker, size)) = &laid.bullet
        {
            let mw = self.measure(marker, false, *size);
            self.text_at(&marker.clone(), false, false, *size, x + laid.indent - mw - 5.0, baseline);
        }
        let avail = width - laid.indent;
        let used = line.width - line.segs.last().map_or(0.0, |s| self.measure(&s.text[s.text.trim_end().len()..], s.bold, s.size));
        let mut cx = x
            + laid.indent
            + match laid.align {
                Align::Left => 0.0,
                Align::Center => ((avail - used) / 2.0).max(0.0),
                Align::Right => (avail - used).max(0.0),
            };
        for seg in line.segs.clone() {
            self.text_at(&seg.text, seg.bold, seg.italic, seg.size, cx, baseline);
            cx += seg.width;
        }
    }

    /// Embed `picture`: the image object and its size in points (the document's, or the
    /// image's own). `None`, counted as skipped, when the image can't be read (EMF, SVG…).
    fn picture_xobject(&mut self, picture: &Picture) -> Option<(pdfcraft_cos::ObjRef, (f64, f64))> {
        match image_xobject(&mut self.doc, "picture", &picture.data) {
            Ok((r, natural)) => {
                let size = picture.size.filter(|(w, h)| *w > 0.5 && *h > 0.5 && w.is_finite() && h.is_finite()).unwrap_or(natural);
                Some((r, size))
            }
            Err(_) => {
                self.report.skipped = self.report.skipped.saturating_add(1);
                None
            }
        }
    }

    /// Draw image `r` on the current page in `x, y, w, h` (PDF coordinates, y up).
    fn draw_image(&mut self, r: pdfcraft_cos::ObjRef, x: f64, y: f64, w: f64, h: f64) {
        self.image_count = self.image_count.saturating_add(1);
        let name = format!("Im{}", self.image_count);
        let op = format!("q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q\n");
        if let Some(p) = self.pages.last_mut() {
            p.images.set(name.as_bytes().to_vec(), Object::Ref(r));
            p.content.extend_from_slice(op.as_bytes());
        }
    }

    fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        let op = format!("q 0.6 G 0.5 w {x:.2} {y:.2} {w:.2} {h:.2} re S Q\n");
        if let Some(p) = self.pages.last_mut() {
            p.content.extend_from_slice(op.as_bytes());
        }
    }

    // ── flowing documents ──

    fn flow(&mut self, flow: Flow) -> Result<(), CreateError> {
        self.report.skipped = self.report.skipped.saturating_add(flow.skipped);
        let (pw, ph) = flow.page;
        let valid = |v: f64, max: f64| v.is_finite() && v > 0.0 && v <= max;
        let (pw, ph) = if valid(pw, crate::MAX_SIDE) && valid(ph, crate::MAX_SIDE) && pw >= 72.0 && ph >= 72.0 { (pw, ph) } else { A4 };
        let [mut mt, mut mr, mut mb, mut ml] = flow.margins.map(|m| if m.is_finite() { m.clamp(0.0, 288.0) } else { 72.0 });
        if ml + mr > pw - 72.0 {
            (ml, mr) = (36.0, 36.0);
        }
        if mt + mb > ph - 72.0 {
            (mt, mb) = (36.0, 36.0);
        }
        let width = pw - ml - mr;
        let mut f = Cursor { top: ph - mt, bottom: mb, y: ph - mt, fresh: true };
        self.page((pw, ph))?;
        for block in flow.blocks {
            match block {
                Block::PageBreak => {
                    if !f.fresh {
                        self.page((pw, ph))?;
                        f.reset();
                    }
                }
                Block::Para(para) => self.flow_para(&para, ml, width, 11.0, (pw, ph), &mut f)?,
                Block::Picture(picture) => {
                    let Some((r, (w, h))) = self.picture_xobject(&picture) else { continue };
                    let scale = (width / w).min((f.top - f.bottom) / h).min(1.0);
                    let (w, h) = (w * scale, h * scale);
                    if f.y - h < f.bottom && !f.fresh {
                        self.page((pw, ph))?;
                        f.reset();
                    }
                    self.draw_image(r, ml, f.y - h, w, h);
                    f.y -= h + 8.0;
                    f.fresh = false;
                }
                Block::Table(table) => self.flow_table(&table, ml, width, (pw, ph), &mut f)?,
            }
        }
        Ok(())
    }

    fn flow_para(&mut self, para: &Para, x: f64, width: f64, base: f64, page: (f64, f64), f: &mut Cursor) -> Result<(), CreateError> {
        if para.is_empty() {
            let gap = para.runs.iter().filter_map(|r| r.size).fold(base, f64::max) * 1.0;
            f.y -= gap;
            if f.y < f.bottom {
                self.page(page)?;
                f.reset();
            }
            return Ok(());
        }
        let laid = self.lay(para, width, base, 1.0);
        if !f.fresh {
            f.y -= laid.before;
        }
        for i in 0..laid.lines.len() {
            let h = laid.lines.get(i).map_or(0.0, Line::height);
            // Keep a heading with the line after it.
            let need = if para.heading.is_some() && i == 0 { h * 2.5 } else { h };
            if f.y - need < f.bottom && !f.fresh {
                self.page(page)?;
                f.reset();
            }
            self.draw_line(&laid, i, x, f.y, width);
            f.y -= h;
            f.fresh = false;
        }
        f.y -= laid.after;
        Ok(())
    }

    /// Column x-offsets and widths for `table` across `width`.
    fn columns(table: &Table, width: f64) -> Vec<f64> {
        let n = table.rows.iter().map(|r| r.iter().map(|c| c.span.max(1)).sum::<usize>()).max().unwrap_or(1).max(table.widths.len()).max(1);
        let given: Vec<f64> = table.widths.iter().copied().filter(|w| w.is_finite() && *w > 0.0).collect();
        let raw: Vec<f64> = if given.len() == n { given } else { vec![1.0; n] };
        let total: f64 = raw.iter().sum::<f64>().max(f64::EPSILON);
        raw.iter().map(|w| w / total * width).collect()
    }

    /// Lay out a row: each cell's x, width and paragraphs.
    fn lay_row(&mut self, row: &[Cell], cols: &[f64], x: f64, base: f64, scale: f64) -> (Vec<(f64, f64, Vec<Laid>)>, f64) {
        const PAD: f64 = 4.0;
        let mut cells = Vec::new();
        let mut col = 0usize;
        let mut cx = x;
        let mut height: f64 = 0.0;
        for cell in row {
            let span = cell.span.max(1);
            let w: f64 = cols.iter().skip(col).take(span).sum();
            col = col.saturating_add(span);
            if w <= 0.0 {
                continue;
            }
            let laid: Vec<Laid> = cell.paras.iter().filter(|p| !p.is_empty()).map(|p| self.lay(p, (w - 2.0 * PAD).max(12.0), base, scale)).collect();
            let h: f64 = laid.iter().map(|l| l.height() - l.after - l.before).sum::<f64>() + 2.0 * PAD;
            height = height.max(h);
            cells.push((cx, w, laid));
            cx += w;
        }
        (cells, height.max(base * scale * 1.3 + 2.0 * PAD))
    }

    fn draw_row(&mut self, cells: &[(f64, f64, Vec<Laid>)], top: f64, height: f64) {
        const PAD: f64 = 4.0;
        for (cx, w, laid) in cells {
            self.stroke_rect(*cx, top - height, *w, height);
            let mut y = top - PAD;
            for l in laid {
                for i in 0..l.lines.len() {
                    self.draw_line(l, i, cx + PAD, y, w - 2.0 * PAD);
                    y -= l.lines.get(i).map_or(0.0, Line::height);
                }
            }
        }
    }

    fn flow_table(&mut self, table: &Table, x: f64, width: f64, page: (f64, f64), f: &mut Cursor) -> Result<(), CreateError> {
        let cols = Self::columns(table, width);
        for row in &table.rows {
            let (cells, height) = self.lay_row(row, &cols, x, 10.0, 1.0);
            if f.y - height < f.bottom && !f.fresh {
                self.page(page)?;
                f.reset();
            }
            if height > f.top - f.bottom {
                // Taller than a page: its cells' text flows as paragraphs instead, so none is lost.
                for cell in row {
                    for p in &cell.paras {
                        self.flow_para(p, x, width, 10.0, page, f)?;
                    }
                }
                continue;
            }
            self.draw_row(&cells, f.y, height);
            f.y -= height;
            f.fresh = false;
        }
        f.y -= 8.0;
        Ok(())
    }

    // ── slides ──

    fn deck(&mut self, deck: Deck) -> Result<(), CreateError> {
        self.report.skipped = self.report.skipped.saturating_add(deck.skipped);
        let (w, h) = deck.size;
        let (w, h) = if w.is_finite() && h.is_finite() && (72.0..=crate::MAX_SIDE).contains(&w) && (72.0..=crate::MAX_SIDE).contains(&h) {
            (w, h)
        } else {
            (720.0, 405.0)
        };
        if deck.slides.is_empty() {
            return self.page((w, h));
        }
        for slide in deck.slides {
            self.page((w, h))?;
            for item in slide {
                match item {
                    Item::Text { rect, paras, size } => self.slide_text(rect, &paras, size, h),
                    Item::Picture { rect, picture } => {
                        let Some((r, _)) = self.picture_xobject(&picture) else { continue };
                        let [x, y, rw, rh] = rect;
                        self.draw_image(r, x, h - y - rh, rw.max(1.0), rh.max(1.0));
                    }
                    Item::Table { rect, table } => {
                        let [x, y, rw, _] = rect;
                        let cols = Self::columns(&table, rw.max(36.0));
                        let mut top = h - y;
                        for row in &table.rows {
                            let (cells, height) = self.lay_row(row, &cols, x, 14.0, 1.0);
                            self.draw_row(&cells, top, height);
                            top -= height;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// A slide's text box: shrunk (down to 40 %) until it fits, as PowerPoint's autofit does.
    fn slide_text(&mut self, rect: Rect, paras: &[Para], size: f64, page_h: f64) {
        const INSET: (f64, f64) = (7.2, 3.6);
        let [x, y, w, h] = rect;
        let width = (w - 2.0 * INSET.0).max(24.0);
        let budget = self.text_budget;
        let mut laid = Vec::new();
        for step in 0..=6 {
            let scale = 1.0 - 0.1 * f64::from(step);
            self.text_budget = budget;
            laid = paras.iter().map(|p| self.lay(p, width, size, scale)).collect();
            let total: f64 = laid.iter().map(Laid::height).sum();
            if total <= h - 2.0 * INSET.1 + 1.0 {
                break;
            }
        }
        let mut top = page_h - y - INSET.1;
        for l in &laid {
            if l.lines.iter().all(|line| line.segs.is_empty()) && l.bullet.is_none() {
                top -= l.lines.first().map_or(size, Line::height);
                continue;
            }
            top -= l.before;
            for i in 0..l.lines.len() {
                self.draw_line(l, i, x + INSET.0, top, width);
                top -= l.lines.get(i).map_or(0.0, Line::height);
            }
            top -= l.after;
        }
    }

    fn finish(mut self, title: &str) -> Result<(Document, OfficeReport), CreateError> {
        if self.pages.is_empty() {
            self.page(A4)?;
        }
        let fonts = Object::Ref(self.doc.add(Object::Dict(self.fonts.clone())));
        for p in std::mem::take(&mut self.pages) {
            let mut res = Dict::new();
            res.set(b"Font".to_vec(), fonts.clone());
            if !p.images.is_empty() {
                res.set(b"XObject".to_vec(), Object::Dict(p.images));
            }
            add_page(&mut self.doc, p.size.0, p.size.1, res, Some(p.content))?;
        }
        set_title(&mut self.doc, title);
        Ok((self.doc, self.report))
    }
}

/// Where the next line goes on a flowing page.
struct Cursor {
    top: f64,
    bottom: f64,
    y: f64,
    /// Nothing drawn on the page yet.
    fresh: bool,
}

impl Cursor {
    fn reset(&mut self) {
        self.y = self.top;
        self.fresh = true;
    }
}

#[cfg(test)]
mod tests;
