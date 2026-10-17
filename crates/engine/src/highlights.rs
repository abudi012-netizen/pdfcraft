//! Collect highlights: the text under every highlight in a document, in page order, so a student
//! can revise from what they marked. A highlight stores only where it is (its quads), so the text
//! is read back from the page: every glyph whose centre falls inside one of its quads.

use std::sync::Arc;

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind, TextGlyph};

use crate::{DocId, Session};

/// One highlight and the text it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    /// 0-based page index.
    pub page: usize,
    /// The highlighted text (empty when the page has no text there, a scan without OCR).
    pub text: String,
    /// The note typed on the highlight, if any.
    pub note: Option<String>,
}

/// Most highlights collected from one document.
const MAX: usize = 5000;

/// Whether point `p` lies inside the convex quad `q` (four corners in order, either winding).
fn inside(q: [[f32; 2]; 4], p: [f32; 2]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if !cross.is_finite() {
            return false;
        }
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    sign != 0.0
}

/// The text of the glyphs (in their reading order) whose centres lie in any of `quads` (view space).
pub fn text_in(glyphs: &[TextGlyph], quads: &[[[f32; 2]; 4]]) -> String {
    let mut out = String::new();
    for g in glyphs {
        let c = [(g.rect[0] + g.rect[2]) / 2.0, (g.rect[1] + g.rect[3]) / 2.0];
        if quads.iter().any(|q| inside(*q, c)) {
            out.push_str(&g.text);
        }
    }
    // Lines break where the glyphs did; spaces collapse.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Session {
    /// Every highlight in document `id`, page by page, with the text it covers.
    pub fn highlights(&self, id: DocId) -> Vec<Highlight> {
        let Some(doc) = self.get(id) else { return Vec::new() };
        let marks: Vec<_> = doc.info.annotations.iter().filter(|a| a.subtype == "Highlight" && a.in_reply_to.is_none()).take(MAX).collect();
        if marks.is_empty() {
            return Vec::new();
        }
        let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
        let mut r = PageRenderer::new(doc.bytes.clone(), config);
        let mut pages: Vec<usize> = marks.iter().map(|a| a.page).collect();
        pages.sort_unstable();
        pages.dedup();
        let mut out = Vec::new();
        for page in pages {
            let Some(info) = doc.info.pages.get(page) else { continue };
            let page_text = r.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text;
            let glyphs: &[TextGlyph] = page_text.as_deref().map_or(&[], |t| t.glyphs.as_slice());
            let mut on_page: Vec<_> = marks.iter().filter(|a| a.page == page).collect();
            // Top to bottom, as the page reads.
            on_page.sort_by(|a, b| b.rect[3].total_cmp(&a.rect[3]));
            for a in on_page {
                // Quads are four user-space points: [x1 y1 x2 y2 x3 y3 x4 y4] (§12.5.6.10), in the
                // order upper-left, upper-right, lower-left, lower-right; reordered round the edge.
                let quads: Vec<[[f32; 2]; 4]> = a
                    .quads
                    .iter()
                    .map(|q| {
                        let v = |i: usize| info.user_to_view(q[2 * i], q[2 * i + 1]);
                        [v(0), v(1), v(3), v(2)]
                    })
                    .collect();
                let quads = if quads.is_empty() {
                    let [x0, y0, x1, y1] = a.rect;
                    vec![[info.user_to_view(x0, y1), info.user_to_view(x1, y1), info.user_to_view(x1, y0), info.user_to_view(x0, y0)]]
                } else {
                    quads
                };
                let note = a.contents.as_ref().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
                out.push(Highlight { page, text: text_in(glyphs, &quads), note });
            }
        }
        out
    }
}

/// The highlights as plain text, one per paragraph, under page headings, for copying or saving.
pub fn as_text(title: &str, highlights: &[Highlight]) -> String {
    let mut s = format!("Highlights — {title}\n");
    let mut last = None;
    for h in highlights {
        if last != Some(h.page) {
            s.push_str(&format!("\nPage {}\n", h.page + 1));
            last = Some(h.page);
        }
        s.push_str("• ");
        s.push_str(if h.text.is_empty() { "(no text)" } else { &h.text });
        if let Some(n) = &h.note {
            s.push_str(&format!("\n  Note: {n}"));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x: f32, y: f32) -> TextGlyph {
        let rect = [x, y, x + 5.0, y + 10.0];
        TextGlyph { text: text.into(), rect, quad: [[x, y + 10.0], [x + 5.0, y + 10.0], [x + 5.0, y], [x, y]], direction: [1.0, 0.0] }
    }

    #[test]
    fn only_glyphs_under_the_quad_are_read() {
        let glyphs = [glyph("a", 0.0, 0.0), glyph("b", 5.0, 0.0), glyph(" ", 10.0, 0.0), glyph("c", 15.0, 0.0), glyph("z", 50.0, 0.0)];
        let quad = [[0.0, 0.0], [20.0, 0.0], [20.0, 10.0], [0.0, 10.0]];
        assert_eq!(text_in(&glyphs, &[quad]), "ab c");
        // Either winding reads the same.
        let reversed = [[0.0, 0.0], [0.0, 10.0], [20.0, 10.0], [20.0, 0.0]];
        assert_eq!(text_in(&glyphs, &[reversed]), "ab c");
    }

    #[test]
    fn degenerate_quads_read_nothing() {
        let glyphs = [glyph("a", 0.0, 0.0)];
        assert_eq!(text_in(&glyphs, &[[[0.0, 0.0]; 4]]), "");
        assert_eq!(text_in(&glyphs, &[[[f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]), "");
        assert_eq!(text_in(&glyphs, &[]), "");
    }

    #[test]
    fn plain_text_groups_by_page() {
        let hs = [
            Highlight { page: 0, text: "Falling film".into(), note: None },
            Highlight { page: 0, text: "Climbing film".into(), note: Some("exam".into()) },
            Highlight { page: 2, text: String::new(), note: None },
        ];
        let s = as_text("Lecture 4", &hs);
        assert_eq!(s, "Highlights — Lecture 4\n\nPage 1\n• Falling film\n• Climbing film\n  Note: exam\n\nPage 3\n• (no text)\n");
    }
}
