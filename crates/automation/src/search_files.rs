//! Search in multiple PDFs as a tool (AGENTS.md §3): Edit ▸ Search in multiple PDFs without the
//! GUI. The files are read from disk and need not be open; nothing is written.

use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError};
use pdfcraft_render::search_files::{extract_file_text, find_in_file};

/// Most files one call searches.
const MAX_FILES: usize = 500;

impl Automation {
    pub(crate) fn text_find_files(&mut self, a: &Args) -> Result<Value> {
        let query = a.str("query")?;
        if query.trim().is_empty() {
            return Err(ToolError::InvalidArgs("query must contain a word or phrase".into()));
        }
        let case_sensitive = a.opt_bool("case_sensitive")?.unwrap_or(false);
        let whole_words = a.opt_bool("whole_words")?.unwrap_or(false);
        let limit = usize::try_from(a.opt_int("limit")?.unwrap_or(500).max(1)).unwrap_or(usize::MAX);
        let paths = a.strs("paths")?;
        if paths.is_empty() {
            return Err(ToolError::InvalidArgs("paths must list at least one PDF".into()));
        }
        if paths.len() > MAX_FILES {
            return Err(ToolError::InvalidArgs(format!("paths lists {} files; at most {MAX_FILES} can be searched at once", paths.len())));
        }
        // A path outside the root refuses the whole call before anything is read, as in the
        // other multi-file tools; a missing or unreadable file is that file's error below.
        let resolved = paths.iter().map(|p| self.resolve(p, false)).collect::<Result<Vec<_>>>()?;
        let mut files = Vec::with_capacity(paths.len());
        let mut total = 0usize;
        let mut capped = false;
        for (path, file) in paths.iter().zip(resolved) {
            let text =
                std::fs::read(&file).map_err(|e| e.to_string()).and_then(|bytes| extract_file_text(std::sync::Arc::new(bytes), None, &|| false));
            let text = match text {
                Ok(text) => text,
                Err(error) => {
                    files.push(json!({ "path": path, "error": error }));
                    continue;
                }
            };
            let room = limit.saturating_sub(total);
            // One past the room proves that matches were left out.
            let hits = find_in_file(&text, query, case_sensitive, whole_words, room.saturating_add(1));
            if hits.len() > room {
                capped = true;
            }
            let matches: Vec<Value> =
                hits.iter().take(room).map(|h| json!({ "page": h.page + 1, "text": h.text, "before": h.before, "after": h.after })).collect();
            total = total.saturating_add(matches.len());
            files.push(json!({
                "path": path,
                "pages": text.pages.len(),
                "count": matches.len(),
                "matches": matches,
                "no_text": text.has_no_text(),
                "failed_pages": text.failed_pages(),
                "truncated_pages": text.truncated,
            }));
        }
        Ok(json!({ "query": query, "count": total, "complete": !capped, "files": files }))
    }
}
