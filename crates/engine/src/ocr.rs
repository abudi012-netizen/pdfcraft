//! Scan & OCR ▸ Recognize text: render pages, read the words in them and add an invisible text
//! layer ([`Edit::AddOcrText`]), making scanned pages searchable and selectable.
//!
//! Recognition is slow (about a second a page), so it is split from the edit: [`Session::ocr_job`]
//! captures what it needs, [`OcrJob::run`] works anywhere (the UI runs it on a worker thread) and
//! [`Session::apply_ocr`] applies the result as one undoable step.

use std::sync::{Arc, Mutex};

use pdfcraft_render::{PageInfo, PageRenderer, RenderConfig, RenderRequest, RequestKind};

pub use pdfcraft_ocr::{LANGUAGES, Models, Ocr, OcrError, PlacedWord};

use crate::{DocId, Edit, EditError, Session};

/// Recognize Text settings.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrSettings {
    /// Resolution pages are rendered at for recognition (Acrobat's "Downsample to" choices).
    pub dpi: f32,
    /// Language code from [`LANGUAGES`] (the models read the Latin alphabet, whatever this says).
    pub language: String,
    /// Leave pages that already have text alone (Acrobat reports "page contains renderable
    /// text" and skips them). When `false`, such pages are read too (a picture on a page that
    /// also has text, #1028) and only the words that aren't already there are added.
    pub skip_text_pages: bool,
}

/// A recognised word whose box is at least this much covered by the page's own text is that
/// text read again, not a word in a picture.
const ALREADY_ON_PAGE: f32 = 0.5;

/// How much of `word` the `text` boxes cover, from 0 to 1. Boxes are `[x0, y0, x1, y1]` in page
/// view space. Overlaps between the text boxes are counted twice (glyphs rarely overlap), so the
/// sum is capped at the whole word.
fn covered(word: [f32; 4], text: &[[f32; 4]]) -> f32 {
    // `max(0.0)` also turns a NaN side into 0: a degenerate box covers nothing.
    let area = |[x0, y0, x1, y1]: [f32; 4]| (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let total = area(word);
    if !total.is_finite() || total <= 0.0 {
        return 0.0;
    }
    let inside: f32 =
        text.iter().map(|g| area([word[0].max(g[0]), word[1].max(g[1]), word[2].min(g[2]), word[3].min(g[3])])).filter(|a| a.is_finite()).sum();
    (inside / total).min(1.0)
}

impl Default for OcrSettings {
    fn default() -> Self {
        OcrSettings { dpi: 300.0, language: "en".into(), skip_text_pages: true }
    }
}

/// What recognition found on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrPage {
    pub page: usize,
    pub words: Vec<PlacedWord>,
    /// Words recognised but left out because the page's own text already has them there.
    pub already_on_page: usize,
    /// Why the page was not read, if it was skipped.
    pub skipped: Option<String>,
}

impl OcrPage {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// The recogniser, loaded once (it takes a moment) and shared.
pub fn engine() -> Result<Arc<Ocr>, String> {
    static OCR: Mutex<Option<Arc<Ocr>>> = Mutex::new(None);
    let mut slot = OCR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = slot.as_ref() {
        return Ok(o.clone());
    }
    let o = Arc::new(Ocr::find().map_err(|e| e.to_string())?);
    *slot = Some(o.clone());
    Ok(o)
}

/// Whether the recognition models are installed.
pub fn available() -> bool {
    Models::find().is_some()
}

/// Everything recognition needs from a document, detached from the session.
pub struct OcrJob {
    bytes: Arc<Vec<u8>>,
    password: Option<String>,
    infos: Vec<PageInfo>,
    /// Per page: the render scale (pixels per point) that shows its images at their own
    /// resolution, if it has any.
    native: Vec<Option<f32>>,
    pub pages: Vec<usize>,
    pub settings: OcrSettings,
}

impl OcrJob {
    /// Read the pages. `progress(done, total)` is called before each page; returning `false`
    /// stops (the pages read so far are returned).
    pub fn run(self, ocr: &Ocr, mut progress: impl FnMut(usize, usize) -> bool) -> Vec<OcrPage> {
        let config = RenderConfig { password: self.password.as_deref().map(Arc::from), ..Default::default() };
        let mut r = PageRenderer::new(self.bytes.clone(), config);
        let mut out = Vec::new();
        let total = self.pages.len();
        for (done, &page) in self.pages.iter().enumerate() {
            if !progress(done, total) {
                break;
            }
            let Some(info) = self.infos.get(page) else { continue };
            let skip = |why: &str| OcrPage { page, words: Vec::new(), already_on_page: 0, skipped: Some(why.into()) };
            // The boxes of the page's own text (an earlier OCR layer's included), in view space.
            let existing: Vec<[f32; 4]> = r
                .render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() })
                .text
                .map(|t| t.glyphs.iter().filter(|g| !g.text.trim().is_empty()).map(|g| g.rect).collect())
                .unwrap_or_default();
            if self.settings.skip_text_pages && !existing.is_empty() {
                out.push(skip("the page already contains text"));
                continue;
            }
            // A scan is read at its own resolution (enlarging it only blurs the letters), at most
            // the chosen one.
            let wanted = self.settings.dpi.clamp(72.0, 600.0) / 72.0;
            let scale = self.native.get(page).copied().flatten().map_or(wanted, |n| n.clamp(1.0, wanted));
            let shot = r.render(RenderRequest { page, scale, ..Default::default() });
            if let Some(e) = shot.error {
                out.push(skip(&e));
                continue;
            }
            let lines = match ocr.recognize(&shot.rgba, shot.width, shot.height) {
                Ok(l) => l,
                Err(e) => {
                    out.push(skip(&e.to_string()));
                    continue;
                }
            };
            let scale = shot.width as f32 / info.width.max(1e-3);
            let to_user = |x: f32, y: f32| {
                let [u, v] = info.view_to_user(x / scale, y / scale);
                [u as f64, v as f64]
            };
            // Words the page's own text already has are not added a second time.
            let (fresh, already): (Vec<_>, Vec<_>) =
                lines.iter().flat_map(|l| &l.words).partition(|w| covered(w.rect.map(|v| v / scale), &existing) < ALREADY_ON_PAGE);
            let words = fresh.into_iter().map(|w| PlacedWord::place(w, to_user)).collect();
            out.push(OcrPage { page, words, already_on_page: already.len(), skipped: None });
        }
        progress(total, total);
        out
    }
}

impl Session {
    /// Capture what recognising `pages` (0-based; empty = all) of document `id` needs.
    pub fn ocr_job(&self, id: DocId, pages: &[usize], settings: OcrSettings) -> Option<OcrJob> {
        let doc = self.get(id)?;
        let all = doc.info.pages.len();
        let pages: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.iter().copied().filter(|p| *p < all).collect() };
        let native = (0..all)
            .map(|p| {
                if !pages.contains(&p) {
                    return None;
                }
                doc.page_images(p)
                    .iter()
                    .filter(|i| i.width > 1 && i.height > 1)
                    .map(|i| {
                        let m = i.matrix;
                        let (w, h) = (m[0].hypot(m[1]), m[2].hypot(m[3]));
                        (i.width as f64 / w.max(1e-6)).max(i.height as f64 / h.max(1e-6)) as f32
                    })
                    .reduce(f32::max)
            })
            .collect();
        Some(OcrJob { bytes: doc.bytes.clone(), password: doc.password.clone(), infos: doc.info.pages.clone(), native, pages, settings })
    }

    /// Add the words found by [`OcrJob::run`] as one undoable step. Returns the number of words.
    pub fn apply_ocr(&mut self, id: DocId, found: &[OcrPage]) -> Result<usize, EditError> {
        let edits: Vec<Edit> =
            found.iter().filter(|p| !p.words.is_empty()).map(|p| Edit::AddOcrText { page: p.page, words: p.words.clone() }).collect();
        let words = found.iter().map(|p| p.words.len()).sum();
        if !edits.is_empty() {
            self.apply(id, Edit::Batch { label: "Recognize text".into(), edits })?;
        }
        Ok(words)
    }

    /// Recognise text on `pages` and add it, in one call (the CLI and agents use this).
    pub fn recognize_text(&mut self, id: DocId, pages: &[usize], settings: OcrSettings) -> Result<Vec<OcrPage>, String> {
        let job = self.ocr_job(id, pages, settings).ok_or("no such document")?;
        if let Some(why) = self.get(id).and_then(|d| d.read_only_reason.clone()) {
            return Err(why);
        }
        let ocr = engine()?;
        let found = job.run(&ocr, |_, _| true);
        self.apply_ocr(id, &found).map_err(|e| e.to_string())?;
        Ok(found)
    }
}

/// What [`recognize_file`] did to one file.
#[derive(Clone, Debug, PartialEq)]
pub struct FileResult {
    /// The new file (incrementally saved), or the original when nothing was recognised.
    pub bytes: Arc<Vec<u8>>,
    pub pages: Vec<OcrPage>,
}

impl FileResult {
    pub fn words(&self) -> usize {
        self.pages.iter().map(|p| p.words.len()).sum()
    }
}

/// Recognize text in multiple files: one PDF's bytes in, the searchable PDF out. `progress`
/// works as for [`OcrJob::run`].
pub fn recognize_file(
    name: &str,
    bytes: Arc<Vec<u8>>,
    password: Option<&str>,
    settings: OcrSettings,
    ocr: &Ocr,
    progress: impl FnMut(usize, usize) -> bool,
) -> Result<FileResult, String> {
    let mut s = Session::new();
    let id = s.open(name, None, bytes.clone(), password).map_err(|e| e.to_string())?;
    if let Some(why) = s.get(id).and_then(|d| d.read_only_reason.clone()) {
        return Err(why);
    }
    let job = s.ocr_job(id, &[], settings).ok_or("the document could not be read")?;
    let pages = job.run(ocr, progress);
    if s.apply_ocr(id, &pages).map_err(|e| e.to_string())? == 0 {
        return Ok(FileResult { bytes, pages });
    }
    Ok(FileResult { bytes: s.save_bytes(id).map_err(|e| e.to_string())?, pages })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1028: a recognised word over the page's own text is that text again; one in an empty
    /// part of the page (a picture) is new.
    #[test]
    fn words_over_existing_text_count_as_already_there() {
        // Glyph em boxes of "Total" at 12 pt, five letters side by side.
        let glyphs: Vec<[f32; 4]> = (0..5).map(|i| [100.0 + 7.0 * i as f32, 50.0, 107.0 + 7.0 * i as f32, 62.0]).collect();
        // OCR's tight box around the same ink.
        assert!(covered([100.5, 52.0, 134.0, 61.0], &glyphs) >= ALREADY_ON_PAGE);
        // A word in a picture further down, and one beside the text on the same line.
        assert_eq!(covered([100.0, 200.0, 140.0, 212.0], &glyphs), 0.0);
        assert_eq!(covered([140.0, 52.0, 170.0, 61.0], &glyphs), 0.0);
        // Touching the text's last letter is not enough.
        assert!(covered([131.0, 52.0, 170.0, 61.0], &glyphs) < ALREADY_ON_PAGE);
        // A page without text has nothing to cover a word.
        assert_eq!(covered([100.5, 52.0, 134.0, 61.0], &[]), 0.0);
    }

    #[test]
    fn coverage_of_overlapping_or_broken_boxes_stays_between_zero_and_one() {
        // A glyph drawn twice (fake bold) does not cover more than the whole word.
        let twice = [[0.0, 0.0, 10.0, 10.0], [0.0, 0.0, 10.0, 10.0]];
        assert_eq!(covered([0.0, 0.0, 10.0, 10.0], &twice), 1.0);
        // Empty, inverted and non-finite boxes cover nothing and are covered by nothing.
        for word in [[5.0, 5.0, 5.0, 9.0], [9.0, 0.0, 1.0, 9.0], [f32::NAN, 0.0, 9.0, 9.0], [0.0, 0.0, f32::INFINITY, 9.0]] {
            assert_eq!(covered(word, &twice), 0.0, "{word:?}");
        }
        let broken = [[f32::NAN; 4], [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY]];
        let c = covered([0.0, 0.0, 10.0, 10.0], &broken);
        assert!((0.0..=1.0).contains(&c), "{c}");
    }
}

/// Auto hide labels: the boxes of every word on a page, found in its text and, when the
/// recognition models are installed, read from its pictures, merged into one box per label.
pub mod hide_labels {
    use super::{Edit, EditError, Ocr, OcrJob, Session, covered};
    use crate::{DocId, NewAnnotation, Shape, Style};
    use pdfcraft_render::{RenderRequest, RequestKind};

    /// What [`find`] found on one page: label boxes in PDF user space `[x0, y0, x1, y1]`.
    #[derive(Clone, Debug, PartialEq)]
    pub struct PageLabels {
        pub page: usize,
        pub boxes: Vec<[f64; 4]>,
    }

    /// Most boxes on one page: past this a page is prose, not a labelled diagram, and merging
    /// stays cheap.
    const MAX_BOXES: usize = 4000;
    /// Points of white around each label, so no edge of a letter shows.
    const PAD: f32 = 1.5;

    /// Whether two boxes (view space, y down) belong to one label: on the same line, and closer
    /// than about a word space.
    fn same_label(a: [f32; 4], b: [f32; 4]) -> bool {
        let h = (a[3] - a[1]).max(b[3] - b[1]).max(0.5);
        let overlap = a[3].min(b[3]) - a[1].max(b[1]);
        let shortest = (a[3] - a[1]).min(b[3] - b[1]).max(0.1);
        let gap = (a[0].max(b[0]) - a[2].min(b[2])).max(0.0);
        overlap >= 0.5 * shortest && gap <= 0.8 * h
    }

    /// Merges glyph and word boxes into label boxes. Non-finite or empty boxes are dropped.
    pub fn merge(boxes: &[[f32; 4]]) -> Vec<[f32; 4]> {
        let mut out: Vec<[f32; 4]> =
            boxes.iter().copied().filter(|r| r.iter().all(|v| v.is_finite()) && r[2] > r[0] && r[3] > r[1]).take(MAX_BOXES).collect();
        // Join pairs until nothing changes; each join removes a box, so this ends.
        loop {
            let mut joined = false;
            let mut i = 0;
            while i < out.len() {
                let mut j = i + 1;
                while j < out.len() {
                    let (Some(&a), Some(&b)) = (out.get(i), out.get(j)) else { break };
                    if same_label(a, b) {
                        if let Some(slot) = out.get_mut(i) {
                            *slot = [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
                        }
                        out.swap_remove(j);
                        joined = true;
                    } else {
                        j += 1;
                    }
                }
                i += 1;
            }
            if !joined {
                return out;
            }
        }
    }

    impl OcrJob {
        /// Finds the labels on the job's pages. With `ocr`, words in pictures are read too;
        /// without it only the page's own text is found. `progress(done, total)` as in
        /// [`OcrJob::run`].
        pub fn find_labels(self, ocr: Option<&Ocr>, mut progress: impl FnMut(usize, usize) -> bool) -> Vec<PageLabels> {
            let config = pdfcraft_render::RenderConfig { password: self.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
            let mut r = pdfcraft_render::PageRenderer::new(self.bytes.clone(), config);
            let total = self.pages.len();
            let mut out = Vec::new();
            for (done, &page) in self.pages.iter().enumerate() {
                if !progress(done, total) {
                    break;
                }
                let Some(info) = self.infos.get(page) else { continue };
                // The page's own text, in view space.
                let mut found: Vec<[f32; 4]> = r
                    .render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() })
                    .text
                    .map(|t| t.glyphs.iter().filter(|g| !g.text.trim().is_empty()).map(|g| g.rect).collect())
                    .unwrap_or_default();
                if let Some(ocr) = ocr {
                    let wanted = self.settings.dpi.clamp(72.0, 600.0) / 72.0;
                    let scale = self.native.get(page).copied().flatten().map_or(wanted, |n| n.clamp(1.0, wanted));
                    let shot = r.render(RenderRequest { page, scale, ..Default::default() });
                    if shot.error.is_none()
                        && let Ok(lines) = ocr.recognize(&shot.rgba, shot.width, shot.height)
                    {
                        let px = shot.width as f32 / info.width.max(1e-3);
                        let text = found.clone();
                        for w in lines.iter().flat_map(|l| &l.words) {
                            let rect = w.rect.map(|v| v / px.max(1e-3));
                            // Words the text layer already has are found once.
                            if covered(rect, &text) < super::ALREADY_ON_PAGE {
                                found.push(rect);
                            }
                        }
                    }
                }
                let boxes = merge(&found)
                    .into_iter()
                    .map(|[x0, y0, x1, y1]| {
                        let a = info.view_to_user(x0 - PAD, y0 - PAD);
                        let b = info.view_to_user(x1 + PAD, y1 + PAD);
                        [a[0].min(b[0]) as f64, a[1].min(b[1]) as f64, a[0].max(b[0]) as f64, a[1].max(b[1]) as f64]
                    })
                    .collect();
                out.push(PageLabels { page, boxes });
            }
            progress(total, total);
            out
        }
    }

    /// The look of a hidden label: an opaque white box with a white border.
    pub fn style() -> Style {
        Style { color: [1.0, 1.0, 1.0], opacity: 1.0, width: 1.0, fill: Some([1.0, 1.0, 1.0]) }
    }

    /// The text every automatically hidden label carries, so they can be told from hand-drawn ones.
    pub const CONTENTS: &str = "Hidden label";

    /// The one undoable step that covers every label in `found` with a white box, or `None` when
    /// nothing was found.
    pub fn edit(found: &[PageLabels], author: &str) -> Option<Edit> {
        let edits: Vec<Edit> = found
            .iter()
            .flat_map(|p| {
                p.boxes.iter().map(move |&rect| {
                    Edit::AddAnnotation(NewAnnotation {
                        page: p.page,
                        shape: Shape::Rectangle { rect },
                        style: style(),
                        contents: CONTENTS.to_owned(),
                        author: author.to_owned(),
                    })
                })
            })
            .collect();
        (!edits.is_empty()).then(|| Edit::Batch { label: "Auto hide labels".into(), edits })
    }

    impl Session {
        /// Covers every label found by [`OcrJob::find_labels`] with a white box, as one undoable
        /// step. Returns how many labels were hidden.
        pub fn hide_labels(&mut self, id: DocId, found: &[PageLabels], author: &str) -> Result<usize, EditError> {
            let count = found.iter().map(|p| p.boxes.len()).sum();
            if let Some(e) = edit(found, author) {
                self.apply(id, e)?;
            }
            Ok(count)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::merge;

        #[test]
        fn letters_on_a_line_become_one_label() {
            let letters: Vec<[f32; 4]> = (0..5).map(|i| [10.0 + 6.0 * i as f32, 100.0, 15.5 + 6.0 * i as f32, 110.0]).collect();
            assert_eq!(merge(&letters), vec![[10.0, 100.0, 39.5, 110.0]]);
        }

        #[test]
        fn far_words_and_other_lines_stay_apart() {
            let boxes = [[10.0, 100.0, 40.0, 110.0], [200.0, 100.0, 240.0, 110.0], [10.0, 140.0, 40.0, 150.0]];
            assert_eq!(merge(&boxes).len(), 3);
        }

        #[test]
        fn words_a_space_apart_join() {
            let boxes = [[10.0, 100.0, 40.0, 110.0], [44.0, 100.0, 70.0, 110.0]];
            assert_eq!(merge(&boxes), vec![[10.0, 100.0, 70.0, 110.0]]);
        }

        #[test]
        fn broken_boxes_are_dropped() {
            assert!(merge(&[[f32::NAN, 0.0, 1.0, 1.0], [5.0, 5.0, 5.0, 9.0], [1.0, 1.0, 0.0, 0.0]]).is_empty());
            assert!(merge(&[]).is_empty());
        }
    }
}
