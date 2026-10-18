//! PowerPoint (.pptx, ECMA-376 PresentationML): one page per slide (hidden slides left out, as
//! PowerPoint's own PDF export does), each text box, picture and table where it sits on the
//! slide. A placeholder without its own position takes the one its layout or master gives.
//! Text is black on white: slide backgrounds and theme colours are not drawn.

use std::collections::HashMap;

use roxmltree::Node;

use super::docx::{EMU, attr, child, letters, roman};
use super::zip::{Zip, relationships};
use super::{Align, Cell, Deck, Item, Para, Picture, Rect, Run, Table};

/// Most slides converted.
const MAX_SLIDES: usize = 2_000;

fn emu(n: Option<Node>, name: &str) -> Option<f64> {
    n.and_then(|n| attr(n, name)).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()).map(|v| v / EMU)
}

/// A shape's box from its `a:xfrm` (or a graphic frame's `p:xfrm`).
fn xfrm(sp: Node) -> Option<Rect> {
    let x = sp.descendants().find(|n| n.has_tag_name("xfrm"))?;
    let (off, ext) = (child(x, "off"), child(x, "ext"));
    Some([emu(off, "x")?, emu(off, "y")?, emu(ext, "cx")?, emu(ext, "cy")?])
}

/// A placeholder: its type ("title", "body", …) and index.
fn placeholder(sp: Node) -> Option<(String, Option<String>)> {
    let ph = sp.children().find(|c| c.tag_name().name().starts_with("nv")).and_then(|nv| child(nv, "nvPr")).and_then(|p| child(p, "ph"))?;
    Some((attr(ph, "type").unwrap_or("obj").to_owned(), attr(ph, "idx").map(str::to_owned)))
}

/// Title-like placeholder types match each other, as do the body-like ones.
fn same_kind(a: &str, b: &str) -> bool {
    fn kind(t: &str) -> &str {
        match t {
            "title" | "ctrTitle" => "title",
            "body" | "subTitle" | "obj" => "body",
            other => other,
        }
    }
    kind(a) == kind(b)
}

/// The placeholders a layout or master positions: (type, index, box).
fn placeholders(zip: &Zip, part: &str) -> Vec<(String, Option<String>, Rect)> {
    let Ok(text) = zip.text(part) else { return Vec::new() };
    let Ok(tree) = super::xml(&text) else { return Vec::new() };
    tree.descendants()
        .filter(|n| n.has_tag_name("sp"))
        .filter_map(|sp| {
            let (t, idx) = placeholder(sp)?;
            let rect = child(sp, "spPr").and_then(xfrm)?;
            Some((t, idx, rect))
        })
        .collect()
}

/// Where a slide's placeholder sits when it doesn't say: its layout's placeholder with the
/// same index, else the same type, else the master's of the same type.
fn inherited(layout: &[(String, Option<String>, Rect)], master: &[(String, Option<String>, Rect)], t: &str, idx: Option<&str>) -> Option<Rect> {
    idx.and_then(|i| layout.iter().find(|(_, li, _)| li.as_deref() == Some(i)))
        .or_else(|| layout.iter().find(|(lt, _, _)| same_kind(lt, t)))
        .or_else(|| master.iter().find(|(mt, _, _)| same_kind(mt, t)))
        .map(|(_, _, r)| *r)
}

/// Maps a group's child coordinates to the slide's.
#[derive(Clone, Copy)]
struct Transform {
    dx: f64,
    dy: f64,
    sx: f64,
    sy: f64,
}

impl Transform {
    const IDENTITY: Transform = Transform { dx: 0.0, dy: 0.0, sx: 1.0, sy: 1.0 };

    fn apply(self, r: Rect) -> Rect {
        [r[0] * self.sx + self.dx, r[1] * self.sy + self.dy, r[2] * self.sx, r[3] * self.sy]
    }
}

struct Slide<'z> {
    zip: &'z Zip<'z>,
    rels: HashMap<String, (String, String)>,
    layout: Vec<(String, Option<String>, Rect)>,
    master: Vec<(String, Option<String>, Rect)>,
    size: (f64, f64),
    items: Vec<Item>,
    skipped: usize,
}

pub(crate) fn read(bytes: &[u8]) -> Result<Deck, String> {
    let zip = Zip::open(bytes)?;
    let text = zip.text("ppt/presentation.xml").map_err(|_| "this is not a PowerPoint presentation (ppt/presentation.xml is missing)")?;
    let tree = super::xml(&text)?;
    let root = tree.root_element();
    let sz = child(root, "sldSz");
    let size = (emu(sz, "cx").unwrap_or(720.0), emu(sz, "cy").unwrap_or(540.0));
    let rels = relationships(&zip, "ppt/presentation.xml");
    let ids: Vec<&str> =
        child(root, "sldIdLst").map(|l| l.children().filter(|n| n.has_tag_name("sldId")).filter_map(rid).collect()).unwrap_or_default();
    let mut deck = Deck { size, slides: Vec::new(), skipped: 0 };
    for id in ids.into_iter().take(MAX_SLIDES) {
        let Some((_, path)) = rels.get(id) else { continue };
        let Ok(text) = zip.text(path) else {
            deck.skipped = deck.skipped.saturating_add(1);
            continue;
        };
        let Ok(tree) = super::xml(&text) else {
            deck.skipped = deck.skipped.saturating_add(1);
            continue;
        };
        if attr(tree.root_element(), "show") == Some("0") {
            continue;
        }
        let slide_rels = relationships(&zip, path);
        let layout_path = slide_rels.values().find(|(t, _)| t == "slideLayout").map(|(_, p)| p.clone());
        let master_path = layout_path.as_deref().and_then(|l| relationships(&zip, l).into_values().find(|(t, _)| t == "slideMaster").map(|(_, p)| p));
        let mut s = Slide {
            zip: &zip,
            rels: slide_rels,
            layout: layout_path.as_deref().map(|p| placeholders(&zip, p)).unwrap_or_default(),
            master: master_path.as_deref().map(|p| placeholders(&zip, p)).unwrap_or_default(),
            size,
            items: Vec::new(),
            skipped: 0,
        };
        if let Some(tree) = tree.descendants().find(|n| n.has_tag_name("spTree")) {
            s.shapes(tree, Transform::IDENTITY);
        }
        deck.skipped = deck.skipped.saturating_add(s.skipped);
        deck.slides.push(s.items);
    }
    Ok(deck)
}

/// A slide id's relationship id (`r:id`).
fn rid<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == "id" && a.namespace().is_some_and(|ns| ns.contains("relationships"))).map(|a| a.value())
}

impl Slide<'_> {
    fn shapes(&mut self, tree: Node, t: Transform) {
        for n in tree.children().filter(Node::is_element) {
            match n.tag_name().name() {
                "sp" => self.shape(n, t),
                "pic" => self.pic(n, t),
                "graphicFrame" => self.frame(n, t),
                "grpSp" => {
                    let x = child(n, "grpSpPr").and_then(|p| child(p, "xfrm"));
                    let inner = match x {
                        Some(x) => {
                            let (off, ext, choff, chext) = (child(x, "off"), child(x, "ext"), child(x, "chOff"), child(x, "chExt"));
                            let (ox, oy, cx, cy) = (
                                emu(off, "x").unwrap_or(0.0),
                                emu(off, "y").unwrap_or(0.0),
                                emu(ext, "cx").unwrap_or(0.0),
                                emu(ext, "cy").unwrap_or(0.0),
                            );
                            let (chx, chy, chw, chh) = (
                                emu(choff, "x").unwrap_or(0.0),
                                emu(choff, "y").unwrap_or(0.0),
                                emu(chext, "cx").unwrap_or(cx),
                                emu(chext, "cy").unwrap_or(cy),
                            );
                            let sx = if chw > 0.0 { cx / chw } else { 1.0 };
                            let sy = if chh > 0.0 { cy / chh } else { 1.0 };
                            // Child point p → off + (p - chOff) · scale, then the outer group's.
                            let local = Transform { dx: ox - chx * sx, dy: oy - chy * sy, sx, sy };
                            Transform { dx: local.dx * t.sx + t.dx, dy: local.dy * t.sy + t.dy, sx: local.sx * t.sx, sy: local.sy * t.sy }
                        }
                        None => t,
                    };
                    self.shapes(n, inner);
                }
                "AlternateContent" => {
                    if let Some(alt) = child(n, "Fallback").or_else(|| child(n, "Choice")) {
                        self.shapes(alt, t);
                    }
                }
                _ => {}
            }
        }
    }

    fn shape(&mut self, sp: Node, t: Transform) {
        let Some(body) = child(sp, "txBody") else { return };
        let ph = placeholder(sp);
        let kind = ph.as_ref().map(|(k, _)| k.as_str());
        let rect = child(sp, "spPr")
            .and_then(xfrm)
            .map(|r| t.apply(r))
            .or_else(|| ph.as_ref().and_then(|(k, i)| inherited(&self.layout, &self.master, k, i.as_deref())));
        let (w, h) = self.size;
        let rect = rect.unwrap_or(match kind {
            Some("title" | "ctrTitle") => [w * 0.07, h * 0.05, w * 0.86, h * 0.18],
            _ => [w * 0.07, h * 0.25, w * 0.86, h * 0.68],
        });
        let (size, bullets) = match kind {
            Some("ctrTitle") => (40.0, false),
            Some("title") => (32.0, false),
            Some("subTitle") => (22.0, false),
            Some("body" | "obj") => (22.0, true),
            _ => (18.0, false),
        };
        let mut paras = text_body(body, bullets);
        if kind == Some("ctrTitle") || kind == Some("subTitle") {
            for p in &mut paras {
                if p.align == Align::Left {
                    p.align = Align::Center;
                }
            }
        }
        if paras.iter().any(|p| !p.text().trim().is_empty()) {
            self.items.push(Item::Text { rect, paras, size });
        }
    }

    fn pic(&mut self, pic: Node, t: Transform) {
        let picture = pic
            .descendants()
            .find(|d| d.has_tag_name("blip"))
            .and_then(|b| attr(b, "embed"))
            .and_then(|id| self.rels.get(id))
            .and_then(|(_, path)| self.zip.read(path).ok());
        let rect = child(pic, "spPr")
            .and_then(xfrm)
            .map(|r| t.apply(r))
            .or_else(|| placeholder(pic).and_then(|(k, i)| inherited(&self.layout, &self.master, &k, i.as_deref())));
        match (picture, rect) {
            (Some(data), Some(rect)) => self.items.push(Item::Picture { rect, picture: Picture { data, size: Some((rect[2], rect[3])) } }),
            _ => self.skipped = self.skipped.saturating_add(1),
        }
    }

    fn frame(&mut self, f: Node, t: Transform) {
        let rect = child(f, "xfrm").and_then(|x| {
            let (off, ext) = (child(x, "off"), child(x, "ext"));
            Some(t.apply([emu(off, "x")?, emu(off, "y")?, emu(ext, "cx")?, emu(ext, "cy")?]))
        });
        let Some(tbl) = f.descendants().find(|n| n.has_tag_name("tbl")) else {
            // A chart, diagram or embedded object: drawn by PowerPoint, not here.
            self.skipped = self.skipped.saturating_add(1);
            return;
        };
        let widths = child(tbl, "tblGrid")
            .map(|g| g.children().filter(|c| c.has_tag_name("gridCol")).map(|c| emu(Some(c), "w").unwrap_or(0.0)).collect())
            .unwrap_or_default();
        let mut rows = Vec::new();
        for tr in tbl.children().filter(|n| n.has_tag_name("tr")) {
            let mut row = Vec::new();
            for tc in tr.children().filter(|n| n.has_tag_name("tc")) {
                if matches!(attr(tc, "hMerge"), Some("1" | "true")) {
                    continue;
                }
                let span = attr(tc, "gridSpan").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                let paras = child(tc, "txBody").map(|b| text_body(b, false)).unwrap_or_default();
                row.push(Cell { paras, span });
            }
            rows.push(row);
        }
        let (w, h) = self.size;
        let rect = rect.unwrap_or([w * 0.07, h * 0.25, w * 0.86, h * 0.68]);
        self.items.push(Item::Table { rect, table: Table { widths, rows } });
    }
}

/// The paragraphs of a DrawingML text body. `bullets`: paragraphs get a bullet unless they say
/// otherwise (a body placeholder's default).
fn text_body(body: Node, bullets: bool) -> Vec<Para> {
    let scale = body
        .descendants()
        .find(|n| n.has_tag_name("normAutofit"))
        .and_then(|a| attr(a, "fontScale"))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or(1.0, |v| (v / 100_000.0).clamp(0.1, 1.0));
    let mut counters: HashMap<u8, u32> = HashMap::new();
    let mut out = Vec::new();
    for p in body.children().filter(|n| n.has_tag_name("p")) {
        let ppr = child(p, "pPr");
        let level = ppr.and_then(|pr| attr(pr, "lvl")).and_then(|l| l.parse::<u8>().ok()).unwrap_or(0).min(8);
        let mut para = Para {
            level,
            align: match ppr.and_then(|pr| attr(pr, "algn")) {
                Some("ctr") => Align::Center,
                Some("r") => Align::Right,
                _ => Align::Left,
            },
            ..Default::default()
        };
        for n in p.children().filter(Node::is_element) {
            match n.tag_name().name() {
                "r" | "fld" => {
                    let rpr = child(n, "rPr");
                    let flag = |name: &str| rpr.and_then(|r| attr(r, name)).is_some_and(|v| v == "1" || v == "true");
                    let size = rpr
                        .and_then(|r| attr(r, "sz"))
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|s| s / 100.0 * scale)
                        .filter(|s| (1.0..=400.0).contains(s));
                    let mut text = child(n, "t").and_then(|t| t.text()).unwrap_or("").to_owned();
                    if rpr.and_then(|r| attr(r, "cap")) == Some("all") {
                        text = text.to_uppercase();
                    }
                    para.runs.push(Run { text, bold: flag("b"), italic: flag("i"), size });
                }
                "br" => para.runs.push(Run::plain("\n")),
                _ => {}
            }
        }
        let has_text = !para.text().trim().is_empty();
        let none = ppr.is_some_and(|pr| child(pr, "buNone").is_some());
        if has_text && !none {
            if let Some(c) = ppr.and_then(|pr| child(pr, "buChar")).and_then(|b| attr(b, "char")).and_then(|c| c.chars().next()) {
                let font = ppr.and_then(|pr| child(pr, "buFont")).and_then(|f| attr(f, "typeface"));
                para.bullet = Some(super::bullet_char(c, font));
            } else if let Some(auto) = ppr.and_then(|pr| child(pr, "buAutoNum")) {
                let n = counters.entry(level).or_insert(0);
                *n = n.saturating_add(1);
                let n = *n;
                let kind = attr(auto, "type").unwrap_or("arabicPeriod");
                let value = if kind.starts_with("alphaLc") {
                    letters(n, false)
                } else if kind.starts_with("alphaUc") {
                    letters(n, true)
                } else if kind.starts_with("romanLc") {
                    roman(n).to_lowercase()
                } else if kind.starts_with("romanUc") {
                    roman(n)
                } else {
                    n.to_string()
                };
                para.bullet = Some(if kind.ends_with("ParenR") {
                    format!("{value})")
                } else if kind.ends_with("ParenBoth") {
                    format!("({value})")
                } else {
                    format!("{value}.")
                });
            } else if bullets {
                para.bullet = Some(if level == 0 { "•" } else { "–" }.into());
            }
        }
        if para.bullet.is_none() {
            counters.remove(&level);
        }
        if bullets {
            // Deeper outline levels are set smaller, as PowerPoint's default body styles are.
            for r in &mut para.runs {
                if r.size.is_none() {
                    r.size = Some(((22.0 - 2.0 * f64::from(level)) * scale).max(10.0));
                }
            }
        }
        out.push(para);
    }
    out
}
