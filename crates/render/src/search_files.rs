//! Search in multiple PDFs (Acrobat's Advanced Search ▸ "All PDF Documents in…"): extract the
//! text of documents that need not be open, then find a word or phrase in all of them.
//!
//! Extraction is the slow part and searching is cheap, so the two are separate: a caller keeps
//! each file's [`FileText`] and runs [`find_in_file`] again as the query changes. Pages are kept
//! as plain text (the same text [`PageText::plain_text`](crate::PageText::plain_text) returns)
//! rather than glyph boxes, so a few hundred documents fit comfortably in memory.
//!
//! Matching follows [`PageText::find_opts`](crate::PageText::find_opts) exactly — the same case
//! folding, whitespace collapsing, whole-word rule and non-overlapping scan — so a match's
//! [`FileHit::ordinal`] is also its position among the find results on that page once the file
//! is open.

use std::sync::Arc;

use crate::raster::{PageRenderer, RenderConfig, RenderRequest, RequestKind};
use crate::text::fold_case;

/// Most pages read from one file. Longer documents are searched up to here and say so.
pub const MAX_PAGES: usize = 20_000;

/// Characters of context kept on each side of a match.
const CONTEXT: usize = 40;

/// The extracted text of one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileText {
    /// Each page's plain text, or why it couldn't be read.
    pub pages: Vec<Result<String, String>>,
    /// The document has more than [`MAX_PAGES`] pages; the rest weren't read.
    pub truncated: bool,
}

impl FileText {
    /// `true` when no page has any text: typically a scan that needs OCR.
    pub fn has_no_text(&self) -> bool {
        self.pages.iter().all(|p| p.as_ref().map_or(true, |t| t.trim().is_empty()))
    }

    /// Pages whose text couldn't be read.
    pub fn failed_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.is_err()).count()
    }
}

/// One match in a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHit {
    /// 0-based page index.
    pub page: usize,
    /// 0-based position of this match among the matches on its page.
    pub ordinal: usize,
    /// Context before the match, whitespace collapsed (`…` when cut).
    pub before: String,
    /// The matched text as it appears on the page, whitespace collapsed.
    pub text: String,
    /// Context after the match, whitespace collapsed (`…` when cut).
    pub after: String,
}

/// Reads the text of every page of a PDF. `cancelled` is polled between pages; when it returns
/// `true` the work stops and `Err` says so. Never panics: the renderer's own guard turns a crash
/// on one page into that page's error.
pub fn extract_file_text(bytes: Arc<Vec<u8>>, password: Option<Arc<str>>, cancelled: &dyn Fn() -> bool) -> Result<FileText, String> {
    let mut renderer = PageRenderer::new(bytes, RenderConfig { password, ..Default::default() });
    let count = renderer.page_count();
    if count == 0 {
        return Err("it isn't a PDF that can be read (damaged, empty or password-protected)".into());
    }
    let mut text = FileText { pages: Vec::with_capacity(count.min(MAX_PAGES)), truncated: count > MAX_PAGES };
    for page in 0..count.min(MAX_PAGES) {
        if cancelled() {
            return Err("cancelled".into());
        }
        let out = renderer.render(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0, preview: false });
        text.pages.push(match out.error {
            Some(e) => Err(e),
            None => Ok(out.text.map(|t| t.plain_text()).unwrap_or_default()),
        });
    }
    Ok(text)
}

/// Finds `query` in a document's pages, at most `limit` matches, in page order.
pub fn find_in_file(text: &FileText, query: &str, case_sensitive: bool, whole_words: bool, limit: usize) -> Vec<FileHit> {
    let mut hits = Vec::new();
    for (page, content) in text.pages.iter().enumerate() {
        let Ok(content) = content else { continue };
        if hits.len() >= limit {
            break;
        }
        hits.extend(find_in_page(content, page, query, case_sensitive, whole_words, limit - hits.len()));
    }
    hits
}

/// Matches of `query` in one page's plain text. See the module docs for the rules.
pub fn find_in_page(content: &str, page: usize, query: &str, case_sensitive: bool, whole_words: bool, limit: usize) -> Vec<FileHit> {
    let fold = |s: &str| if case_sensitive { s.to_string() } else { fold_case(s) };
    let needle: Vec<char> = fold(query).split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    // Folded characters, each with the byte range of the source character it came from.
    // Whitespace runs collapse to one space, as the query's do.
    let mut chars: Vec<(char, usize, usize)> = Vec::new();
    let mut buf = [0u8; 4];
    for (at, c) in content.char_indices() {
        let end = at + c.len_utf8();
        let folded = fold(c.encode_utf8(&mut buf));
        for f in folded.chars() {
            let f = if f.is_whitespace() { ' ' } else { f };
            if f == ' ' && chars.last().is_none_or(|k| k.0 == ' ') {
                continue;
            }
            chars.push((f, at, end));
        }
    }
    let word = |k: Option<&(char, usize, usize)>| k.is_some_and(|c| c.0.is_alphanumeric());
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        let window = chars.get(i..i + needle.len()).unwrap_or_default();
        let bounded = !whole_words || (!word(i.checked_sub(1).and_then(|p| chars.get(p))) && !word(chars.get(i + needle.len())));
        if bounded && window.iter().map(|c| c.0).eq(needle.iter().copied()) {
            let (start, end) = match (window.first(), window.last()) {
                (Some(first), Some(last)) => (first.1, last.2),
                _ => break, // the window is never empty: `needle` isn't
            };
            out.push(FileHit {
                page,
                ordinal: out.len(),
                before: context_before(content, start),
                text: flatten(content.get(start..end).unwrap_or_default()),
                after: context_after(content, end),
            });
            if out.len() == limit {
                break;
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

fn flatten(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Up to [`CONTEXT`] characters before byte `at` (a char boundary).
fn context_before(content: &str, at: usize) -> String {
    let head = content.get(..at).unwrap_or_default();
    let skip = head.chars().count().saturating_sub(CONTEXT);
    let cut: String = head.chars().skip(skip).collect();
    let flat = flatten(&cut);
    let flat = if head.ends_with(char::is_whitespace) && !flat.is_empty() { format!("{flat} ") } else { flat };
    if skip > 0 { format!("…{flat}") } else { flat }
}

/// Up to [`CONTEXT`] characters after byte `at` (a char boundary).
fn context_after(content: &str, at: usize) -> String {
    let tail = content.get(at..).unwrap_or_default();
    let cut: String = tail.chars().take(CONTEXT).collect();
    let more = tail.chars().nth(CONTEXT).is_some();
    let flat = flatten(&cut);
    let flat = if tail.starts_with(char::is_whitespace) && !flat.is_empty() { format!(" {flat}") } else { flat };
    if more { format!("{flat}…") } else { flat }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(pages: &[&str]) -> FileText {
        FileText { pages: pages.iter().map(|p| Ok(p.to_string())).collect(), truncated: false }
    }

    #[test]
    fn finds_every_match_with_page_ordinal_and_context() {
        let text = file(&["Paracetamol overdose damages the liver.", "Ibuprofen and paracetamol\nalternate. Paracetamol again."]);
        let hits = find_in_file(&text, "paracetamol", false, false, 100);
        let found: Vec<_> = hits.iter().map(|h| (h.page, h.ordinal, h.text.as_str())).collect();
        assert_eq!(found, [(0, 0, "Paracetamol"), (1, 0, "paracetamol"), (1, 1, "Paracetamol")]);
        assert_eq!(hits[1].before, "Ibuprofen and ");
        assert_eq!(hits[1].after, " alternate. Paracetamol again.");
    }

    #[test]
    fn phrases_cross_line_breaks_and_case_sensitivity_is_honoured() {
        let text = file(&["take with\n  food daily"]);
        assert_eq!(find_in_file(&text, "with food", false, false, 9).len(), 1);
        assert_eq!(find_in_file(&text, "With food", true, false, 9).len(), 0);
        assert_eq!(find_in_file(&text, "with food", true, false, 9)[0].text, "with food");
    }

    #[test]
    fn whole_words_and_turkish_i_follow_the_page_find() {
        let text = file(&["dopa and dopamine, İSTANBUL"]);
        assert_eq!(find_in_file(&text, "dopa", false, false, 9).len(), 2);
        assert_eq!(find_in_file(&text, "dopa", false, true, 9).len(), 1);
        assert_eq!(find_in_file(&text, "istanbul", false, false, 9).len(), 1);
    }

    #[test]
    fn limit_and_unreadable_pages() {
        let mut text = file(&["a a a a", "a"]);
        text.pages.insert(1, Err("broken".into()));
        assert_eq!(find_in_file(&text, "a", false, false, 3).len(), 3);
        assert_eq!(find_in_file(&text, "a", false, false, 100).iter().map(|h| h.page).collect::<Vec<_>>(), [0, 0, 0, 0, 2]);
        assert_eq!(text.failed_pages(), 1);
        assert!(find_in_file(&text, "   ", false, false, 9).is_empty());
        assert!(find_in_file(&text, "a", false, false, 0).is_empty());
    }

    #[test]
    fn context_is_cut_at_char_boundaries() {
        let long = format!("{}héllo{}", "é".repeat(60), "ü".repeat(60));
        let hit = &find_in_file(&file(&[&long]), "hello", false, false, 1);
        assert!(hit.is_empty(), "accents are not folded, as in the page find");
        let hit = &find_in_file(&file(&[&long]), "héllo", false, false, 1)[0];
        assert!(hit.before.starts_with('…') && hit.after.ends_with('…'));
        assert_eq!(hit.before.chars().count(), CONTEXT + 1);
    }

    #[test]
    fn no_text_means_a_scan() {
        assert!(file(&["  ", ""]).has_no_text());
        assert!(!file(&["", "x"]).has_no_text());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let never = || false;
        assert!(extract_file_text(Arc::new(b"not a pdf".to_vec()), None, &never).is_err());
        assert!(extract_file_text(Arc::new(Vec::new()), None, &never).is_err());
    }
}
