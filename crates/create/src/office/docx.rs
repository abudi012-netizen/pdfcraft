//! Word (.docx, ECMA-376 WordprocessingML): the body's paragraphs, tables and pictures, with
//! headings from the paragraph styles, lists from the numbering part, and the page size and
//! margins from the last section.

use std::collections::HashMap;

use roxmltree::Node;

use super::zip::{Zip, relationships};
use super::{Align, Block, Cell, Flow, Para, Picture, Run, Table};

/// EMUs per point (DrawingML sizes).
pub(crate) const EMU: f64 = 12_700.0;

/// A WordprocessingML on/off property (`<w:b/>`, `<w:b w:val="0"/>`).
fn on(n: Option<Node>) -> bool {
    n.is_some_and(|n| !matches!(attr(n, "val"), Some("0" | "false" | "off" | "none")))
}

/// An attribute by local name, whatever its namespace (`w:val`).
pub(crate) fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

pub(crate) fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

fn num(n: Option<Node>, name: &str) -> Option<f64> {
    n.and_then(|n| attr(n, name)).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite())
}

/// List formats: (numId, level) → `numFmt` ("bullet", "decimal", "lowerLetter"…) and the
/// bullet character.
struct Numbering {
    formats: HashMap<(String, u8), (String, String)>,
    counters: HashMap<(String, u8), u32>,
}

impl Numbering {
    fn read(zip: &Zip) -> Self {
        let mut formats = HashMap::new();
        if let Ok(text) = zip.text("word/numbering.xml")
            && let Ok(tree) = super::xml(&text)
        {
            let mut abstracts: HashMap<&str, Node> = HashMap::new();
            for a in tree.root_element().children().filter(|n| n.has_tag_name("abstractNum")) {
                if let Some(id) = attr(a, "abstractNumId") {
                    abstracts.insert(id, a);
                }
            }
            for n in tree.root_element().children().filter(|n| n.has_tag_name("num")) {
                let (Some(id), Some(abs)) = (attr(n, "numId"), child(n, "abstractNumId").and_then(|a| attr(a, "val"))) else { continue };
                let Some(a) = abstracts.get(abs) else { continue };
                for lvl in a.children().filter(|l| l.has_tag_name("lvl")) {
                    let Some(level) = attr(lvl, "ilvl").and_then(|v| v.parse::<u8>().ok()) else { continue };
                    let fmt = child(lvl, "numFmt").and_then(|f| attr(f, "val")).unwrap_or("decimal").to_owned();
                    let text = child(lvl, "lvlText").and_then(|f| attr(f, "val")).unwrap_or("").to_owned();
                    let font = child(lvl, "rPr").and_then(|r| child(r, "rFonts")).and_then(|f| attr(f, "ascii").or_else(|| attr(f, "hAnsi")));
                    let text = match (fmt.as_str(), text.chars().next()) {
                        ("bullet", Some(c)) => super::bullet_char(c, font),
                        _ => text,
                    };
                    formats.insert((id.to_owned(), level), (fmt, text));
                }
            }
        }
        Numbering { formats, counters: HashMap::new() }
    }

    /// The marker for the next item of list `id` at `level`.
    fn marker(&mut self, id: &str, level: u8) -> String {
        let (fmt, text) = self.formats.get(&(id.to_owned(), level)).cloned().unwrap_or_else(|| ("bullet".into(), String::new()));
        if fmt == "none" {
            return String::new();
        }
        if fmt == "bullet" {
            // Symbol-font bullets were mapped to their shapes when the numbering was read.
            return if text.is_empty() { "•".into() } else { text };
        }
        let count = self.counters.entry((id.to_owned(), level)).or_insert(0);
        *count = count.saturating_add(1);
        let n = *count;
        // A new item restarts the deeper levels.
        self.counters.retain(|(i, l), _| i != id || *l <= level);
        let value = match fmt.as_str() {
            "lowerLetter" => letters(n, false),
            "upperLetter" => letters(n, true),
            "lowerRoman" => roman(n).to_lowercase(),
            "upperRoman" => roman(n),
            _ => n.to_string(),
        };
        // lvlText is like "%1." or "(%2)": keep its punctuation.
        let pattern = if text.contains('%') { text } else { "%1.".into() };
        let mut out = String::new();
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' && chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
                out.push_str(&value);
            } else {
                out.push(c);
            }
        }
        out
    }
}

pub(crate) fn letters(mut n: u32, upper: bool) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(char::from(b'a' + (n % 26) as u8));
        n /= 26;
    }
    let s: String = s.into_iter().rev().collect();
    if upper { s.to_uppercase() } else { s }
}

pub(crate) fn roman(mut n: u32) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let table = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// Paragraph styles that are headings: style id → level (from the style's name, "heading 2",
/// "Title", so it works whatever the language the ids are in).
fn heading_styles(zip: &Zip) -> HashMap<String, u8> {
    let mut out = HashMap::new();
    let Ok(text) = zip.text("word/styles.xml") else { return out };
    let Ok(tree) = super::xml(&text) else { return out };
    for st in tree.root_element().children().filter(|n| n.has_tag_name("style")) {
        let (Some(id), Some(name)) = (attr(st, "styleId"), child(st, "name").and_then(|n| attr(n, "val"))) else { continue };
        let name = name.to_ascii_lowercase();
        let level = if name == "title" {
            Some(1)
        } else if name == "subtitle" {
            Some(2)
        } else {
            name.strip_prefix("heading").and_then(|l| l.trim().parse::<u8>().ok()).filter(|l| (1..=9).contains(l)).map(|l| l.min(6))
        };
        if let Some(level) = level {
            out.insert(id.to_owned(), level);
        }
    }
    out
}

struct Reader<'z> {
    zip: &'z Zip<'z>,
    rels: HashMap<String, (String, String)>,
    headings: HashMap<String, u8>,
    numbering: Numbering,
    skipped: usize,
}

pub(crate) fn read(bytes: &[u8]) -> Result<Flow, String> {
    let zip = Zip::open(bytes)?;
    if !zip.has("word/document.xml") {
        return Err("this is not a Word document (word/document.xml is missing)".into());
    }
    let text = zip.text("word/document.xml")?;
    let tree = super::xml(&text)?;
    let body = tree.root_element().children().find(|n| n.has_tag_name("body")).ok_or("the document has no body")?;
    let mut r = Reader {
        zip: &zip,
        rels: relationships(&zip, "word/document.xml"),
        headings: heading_styles(&zip),
        numbering: Numbering::read(&zip),
        skipped: 0,
    };
    let mut flow = Flow::new(super::A4);
    if let Some(sect) = body.children().rfind(|n| n.has_tag_name("sectPr")) {
        if let (Some(w), Some(h)) = (num(child(sect, "pgSz"), "w"), num(child(sect, "pgSz"), "h")) {
            flow.page = (w / 20.0, h / 20.0);
        }
        let mar = child(sect, "pgMar");
        if let (Some(t), Some(rt), Some(b), Some(l)) = (num(mar, "top"), num(mar, "right"), num(mar, "bottom"), num(mar, "left")) {
            flow.margins = [t.abs() / 20.0, rt / 20.0, b.abs() / 20.0, l / 20.0];
        }
    }
    r.blocks(body, &mut flow.blocks);
    flow.skipped = r.skipped;
    Ok(flow)
}

impl Reader<'_> {
    fn blocks(&mut self, parent: Node, out: &mut Vec<Block>) {
        for n in parent.children().filter(Node::is_element) {
            match n.tag_name().name() {
                "p" => self.paragraph(n, out),
                "tbl" => out.push(Block::Table(self.table(n))),
                // Content controls, inserted text and custom XML wrap ordinary content.
                "sdt" => {
                    if let Some(c) = child(n, "sdtContent") {
                        self.blocks(c, out);
                    }
                }
                "customXml" | "ins" | "moveTo" | "smartTag" => self.blocks(n, out),
                _ => {}
            }
        }
    }

    fn table(&mut self, tbl: Node) -> Table {
        let widths = child(tbl, "tblGrid")
            .map(|g| g.children().filter(|c| c.has_tag_name("gridCol")).map(|c| num(Some(c), "w").unwrap_or(0.0)).collect())
            .unwrap_or_default();
        let mut rows = Vec::new();
        for tr in tbl.children().filter(|n| n.has_tag_name("tr")) {
            let mut row = Vec::new();
            for tc in tr.children().filter(|n| n.has_tag_name("tc")) {
                let span = child(tc, "tcPr").and_then(|p| num(child(p, "gridSpan"), "val")).map_or(1, |s| (s as usize).clamp(1, 64));
                let mut blocks = Vec::new();
                self.blocks(tc, &mut blocks);
                let mut paras = Vec::new();
                for b in blocks {
                    match b {
                        Block::Para(p) => paras.push(p),
                        // A table in a cell: its text, one paragraph per row.
                        Block::Table(t) => paras.extend(t.rows.into_iter().map(|row| Para {
                            runs: vec![Run::plain(
                                row.iter().map(|c| c.paras.iter().map(Para::text).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(" | "),
                            )],
                            ..Default::default()
                        })),
                        Block::Picture(_) => self.skipped = self.skipped.saturating_add(1),
                        Block::PageBreak => {}
                    }
                }
                row.push(Cell { paras, span });
            }
            rows.push(row);
        }
        Table { widths, rows }
    }

    fn paragraph(&mut self, p: Node, out: &mut Vec<Block>) {
        let ppr = child(p, "pPr");
        let style = ppr.and_then(|pr| child(pr, "pStyle")).and_then(|s| attr(s, "val"));
        let mut para = Para { heading: style.and_then(|s| self.headings.get(s).copied()), ..Default::default() };
        if let Some(pr) = ppr {
            para.align = match child(pr, "jc").and_then(|j| attr(j, "val")) {
                Some("center") => Align::Center,
                Some("right" | "end") => Align::Right,
                _ => Align::Left,
            };
            if let Some(np) = child(pr, "numPr") {
                let level = num(child(np, "ilvl"), "val").map_or(0, |l| l.clamp(0.0, 8.0) as u8);
                if let Some(id) = child(np, "numId").and_then(|n| attr(n, "val")).filter(|id| *id != "0") {
                    let marker = self.numbering.marker(id, level);
                    para.level = level;
                    para.bullet = (!marker.is_empty()).then_some(marker);
                }
            }
            if on(child(pr, "pageBreakBefore")) {
                out.push(Block::PageBreak);
            }
        }
        let mut pictures = Vec::new();
        self.runs(p, &mut para, &mut pictures, out);
        out.push(Block::Para(para));
        out.extend(pictures.into_iter().map(Block::Picture));
    }

    /// The runs under `node` (a paragraph, or a link or field in it); a text box's own
    /// paragraphs are not followed.
    fn runs(&mut self, node: Node, para: &mut Para, pictures: &mut Vec<Picture>, out: &mut Vec<Block>) {
        for n in node.children().filter(Node::is_element) {
            match n.tag_name().name() {
                "r" => self.run(n, para, pictures, out),
                "hyperlink" | "ins" | "moveTo" | "smartTag" | "fldSimple" | "customXml" | "sdtContent" | "dir" | "bdo" => {
                    self.runs(n, para, pictures, out)
                }
                "sdt" => {
                    if let Some(c) = child(n, "sdtContent") {
                        self.runs(c, para, pictures, out);
                    }
                }
                _ => {}
            }
        }
    }

    fn run(&mut self, r: Node, para: &mut Para, pictures: &mut Vec<Picture>, out: &mut Vec<Block>) {
        let rpr = child(r, "rPr");
        let style = |name: &str| on(rpr.and_then(|p| child(p, name)));
        let mut run = Run {
            text: String::new(),
            bold: style("b"),
            italic: style("i"),
            size: num(rpr.and_then(|p| child(p, "sz")), "val").map(|s| s / 2.0).filter(|s| (1.0..=400.0).contains(s)),
        };
        if style("vanish") {
            return;
        }
        let caps = style("caps");
        for n in r.children().filter(Node::is_element) {
            match n.tag_name().name() {
                "t" => run.text.push_str(&n.text().map_or_else(String::new, |t| if caps { t.to_uppercase() } else { t.to_owned() })),
                "tab" | "ptab" => run.text.push('\t'),
                "br" | "cr" => {
                    if attr(n, "type") == Some("page") {
                        // A page break inside the paragraph: what came before ends this page.
                        let next = Run { text: String::new(), ..run.clone() };
                        para.runs.push(std::mem::replace(&mut run, next));
                        let done = std::mem::replace(para, Para { heading: para.heading, align: para.align, ..Default::default() });
                        out.push(Block::Para(done));
                        out.extend(pictures.drain(..).map(Block::Picture));
                        out.push(Block::PageBreak);
                    } else {
                        run.text.push('\n');
                    }
                }
                "noBreakHyphen" => run.text.push('-'),
                "sym" => {
                    if let Some(c) = attr(n, "char")
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .and_then(char::from_u32)
                        .filter(|c| !('\u{e000}'..='\u{f8ff}').contains(c))
                    {
                        run.text.push(c);
                    }
                }
                "drawing" | "pict" | "object" => match self.picture(n) {
                    Some(p) => pictures.push(p),
                    None => self.skipped = self.skipped.saturating_add(1),
                },
                "AlternateContent" => {
                    // Prefer the fallback (a picture) over the choice (a chart or shape).
                    let alt = child(n, "Fallback").or_else(|| child(n, "Choice"));
                    match alt.and_then(|a| self.picture(a)) {
                        Some(p) => pictures.push(p),
                        None => self.skipped = self.skipped.saturating_add(1),
                    }
                }
                _ => {}
            }
        }
        if !run.text.is_empty() {
            para.runs.push(run);
        }
    }

    /// The image a drawing or VML picture shows, with its size on the page.
    fn picture(&self, n: Node) -> Option<Picture> {
        let id = n
            .descendants()
            .find(|d| d.has_tag_name("blip"))
            .and_then(|b| attr(b, "embed"))
            .or_else(|| n.descendants().find(|d| d.has_tag_name("imagedata")).and_then(|i| attr(i, "id")))?;
        let (_, path) = self.rels.get(id)?;
        let data = self.zip.read(path).ok()?;
        let extent = n.descendants().find(|d| d.has_tag_name("extent"));
        let size = match (num(extent, "cx"), num(extent, "cy")) {
            (Some(w), Some(h)) => Some((w / EMU, h / EMU)),
            _ => None,
        };
        Some(Picture { data, size })
    }
}
