//! Edit ▸ Search in multiple PDFs (Acrobat's Advanced Search over "All PDF Documents in…"):
//! add PDFs or a folder, type a word or phrase, and every match in every file is listed with its
//! page and context. Clicking one opens that file (or switches to its tab) at the match, with the
//! search carried into the document's own find so the next and previous matches are one key away.
//!
//! The files are read on a worker thread, one at a time, and only their text is kept
//! ([`pdfcraft_render::search_files::FileText`]), so typing re-searches instantly. Desktop only:
//! the web build has no file paths to come back to.

use std::path::PathBuf;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Align, Color32, FontId, Layout, TextFormat};
use pdfcraft_render::search_files::{FileHit, FileText, find_in_file};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};
use crate::widgets;

/// Most files in one search; more are refused with a message.
const MAX_FILES: usize = 500;
/// Most results listed; the count says when there are more.
const MAX_RESULTS: usize = 2000;
/// PDFs found in a folder are looked for this many levels down.
#[cfg(not(target_arch = "wasm32"))]
const FOLDER_DEPTH: usize = 6;
/// Frames to wait for an opened file's page text before giving up on selecting the exact match
/// (the file is then still open at the right page).
const PENDING_FRAMES: u32 = 900;

/// One file in the search.
struct Entry {
    path: PathBuf,
    name: String,
    state: State,
}

enum State {
    Reading,
    Ready(Arc<FileText>),
    Failed(String),
}

/// The dialog's state, kept while it is closed so reopening keeps the files and the query.
#[derive(Default)]
pub struct SearchFiles {
    pub(crate) open: bool,
    entries: Vec<Entry>,
    pub(crate) query: String,
    pub(crate) case_sensitive: bool,
    pub(crate) whole_words: bool,
    focus: bool,
    /// The results and the inputs they were computed from (query, options, files ready).
    results: Vec<(usize, FileHit)>,
    more: bool,
    key: Option<(String, bool, bool, usize, usize)>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<Worker>,
    /// A result being opened: applied to its document once the page's text is there.
    pending: Option<Pending>,
}

struct Pending {
    path: String,
    page: usize,
    ordinal: usize,
    query: String,
    case_sensitive: bool,
    whole_words: bool,
    started: bool,
    frames: u32,
}

/// Reads queued files on a background thread.
#[cfg(not(target_arch = "wasm32"))]
struct Worker {
    queue: std::sync::mpsc::Sender<(usize, PathBuf)>,
    done: std::sync::mpsc::Receiver<(usize, Result<FileText, String>)>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    fn start(ctx: Option<egui::Context>) -> Option<Self> {
        let (queue, jobs) = std::sync::mpsc::channel::<(usize, PathBuf)>();
        let (send, done) = std::sync::mpsc::channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let halt = stop.clone();
        std::thread::Builder::new()
            .name("search-files".into())
            .spawn(move || {
                while let Ok((i, path)) = jobs.recv() {
                    let cancelled = || halt.load(std::sync::atomic::Ordering::Relaxed);
                    if cancelled() {
                        break;
                    }
                    // Last-resort guard (AGENTS.md §4): one hostile file must not end the search.
                    let result = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
                        pdfcraft_engine::guard(|| pdfcraft_render::search_files::extract_file_text(Arc::new(bytes), None, &cancelled))
                            .unwrap_or_else(|m| Err(format!("an internal error stopped it ({m})")))
                    });
                    if send.send((i, result)).is_err() {
                        break;
                    }
                    if let Some(ctx) = &ctx {
                        ctx.request_repaint();
                    }
                }
            })
            .ok()?;
        Some(Self { queue, done, stop })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SearchFiles {
    /// Adds files (duplicates and non-PDFs are skipped) and starts reading them. Returns how many
    /// were added.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn add(&mut self, paths: Vec<PathBuf>, ctx: Option<egui::Context>) -> usize {
        let mut added = 0;
        for path in paths {
            if self.entries.len() >= MAX_FILES {
                break;
            }
            if !is_pdf(&path) || self.entries.iter().any(|e| e.path == path) {
                continue;
            }
            if self.worker.is_none() {
                self.worker = Worker::start(ctx.clone());
            }
            let index = self.entries.len();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
            let state = match &self.worker {
                Some(w) if w.queue.send((index, path.clone())).is_ok() => State::Reading,
                _ => State::Failed("the background reader couldn't start".into()),
            };
            self.entries.push(Entry { path, name, state });
            added += 1;
        }
        self.key = None;
        added
    }

    /// Takes the files the worker finished.
    #[cfg(not(target_arch = "wasm32"))]
    fn receive(&mut self) {
        let Some(worker) = &self.worker else { return };
        while let Ok((i, result)) = worker.done.try_recv() {
            if let Some(entry) = self.entries.get_mut(i) {
                entry.state = match result {
                    Ok(text) => State::Ready(Arc::new(text)),
                    Err(e) => State::Failed(e),
                };
                self.key = None;
            }
        }
    }

    fn reading(&self) -> usize {
        self.entries.iter().filter(|e| matches!(e.state, State::Reading)).count()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.results.clear();
        self.more = false;
        self.key = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Dropping the worker stops it; a new one starts with the next file.
            self.worker = None;
        }
    }

    /// Runs the search again when the query, its options or the files read changed.
    fn refresh(&mut self) {
        let ready = self.entries.iter().filter(|e| matches!(e.state, State::Ready(_))).count();
        let key = (self.query.clone(), self.case_sensitive, self.whole_words, ready, self.entries.len());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.key = Some(key);
        self.results.clear();
        self.more = false;
        if self.query.trim().is_empty() {
            return;
        }
        for (i, entry) in self.entries.iter().enumerate() {
            let State::Ready(text) = &entry.state else { continue };
            let room = MAX_RESULTS.saturating_sub(self.results.len());
            let hits = find_in_file(text, &self.query, self.case_sensitive, self.whole_words, room.saturating_add(1));
            if hits.len() > room {
                self.more = true;
            }
            self.results.extend(hits.into_iter().take(room).map(|h| (i, h)));
            if self.more {
                break;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn is_pdf(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// The PDFs in `dir` and its subfolders (to a few levels), sorted by path. Unreadable folders
/// are skipped; links aren't followed, so a link loop can't trap the walk.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn pdfs_in_folder(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&folder) else { continue };
        for entry in read.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() && depth < FOLDER_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() && is_pdf(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    out.sort();
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

impl PdfCraftApp {
    /// Edit ▸ Search in multiple PDFs: show the dialog, keeping any files and query from before.
    pub fn open_search_files(&mut self) {
        if cfg!(target_arch = "wasm32") {
            self.notify_tr("Searching in multiple PDFs needs the desktop app.");
            return;
        }
        let state = self.search_files.get_or_insert_with(Default::default);
        state.open = true;
        state.focus = true;
    }

    /// Adds PDFs to the search (tests and automation use this in place of the file picker).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn search_files_add(&mut self, paths: Vec<PathBuf>) {
        let ctx = self.ctx.clone();
        let state = self.search_files.get_or_insert_with(Default::default);
        let before = state.entries.len();
        state.add(paths, ctx);
        if state.entries.len() >= MAX_FILES && state.entries.len() == before {
            self.notify_fmt("At most {n} files can be searched at once.", &[("n", &MAX_FILES.to_string())]);
        }
    }

    /// Sets what to search for and how (tests and automation, in place of typing).
    pub fn search_files_query(&mut self, query: &str, case_sensitive: bool, whole_words: bool) {
        let state = self.search_files.get_or_insert_with(Default::default);
        state.query = query.to_string();
        state.case_sensitive = case_sensitive;
        state.whole_words = whole_words;
    }

    /// `true` while the dialog is showing.
    pub fn search_files_showing(&self) -> bool {
        self.search_files.as_ref().is_some_and(|s| s.open)
    }

    /// The results as (file name, 1-based page, matched text), for tests and automation.
    pub fn search_files_results(&mut self) -> Vec<(String, usize, String)> {
        let Some(state) = self.search_files.as_mut() else { return Vec::new() };
        #[cfg(not(target_arch = "wasm32"))]
        state.receive();
        state.refresh();
        state.results.iter().filter_map(|(i, h)| state.entries.get(*i).map(|e| (e.name.clone(), h.page + 1, h.text.clone()))).collect()
    }

    /// `true` while a result being opened still waits for its document.
    pub fn search_files_pending(&self) -> bool {
        self.search_files.as_ref().is_some_and(|s| s.pending.is_some())
    }

    /// `true` while files are still being read.
    pub fn search_files_busy(&mut self) -> bool {
        let Some(state) = self.search_files.as_mut() else { return false };
        #[cfg(not(target_arch = "wasm32"))]
        state.receive();
        state.reading() > 0
    }

    /// Opens result `index`: switches to the file's tab or opens it, then goes to the match.
    pub fn search_files_open_result(&mut self, index: usize) {
        let Some(state) = self.search_files.as_mut() else { return };
        let Some((file, hit)) = state.results.get(index) else { return };
        let Some(entry) = state.entries.get(*file) else { return };
        let path = entry.path.to_string_lossy().into_owned();
        state.pending = Some(Pending {
            path: path.clone(),
            page: hit.page,
            ordinal: hit.ordinal,
            query: state.query.clone(),
            case_sensitive: state.case_sensitive,
            whole_words: state.whole_words,
            started: false,
            frames: 0,
        });
        state.open = false;
        self.open_recent(&path);
    }

    /// Carries a result being opened into its document: the find is set to the search's query,
    /// the page is shown, and once that page's text is known the exact match is selected. Called
    /// every frame.
    pub(crate) fn search_files_tick(&mut self) {
        let Some(state) = self.search_files.as_mut() else { return };
        #[cfg(not(target_arch = "wasm32"))]
        state.receive();
        let Some(pending) = state.pending.as_mut() else { return };
        pending.frames = pending.frames.saturating_add(1);
        if pending.frames > PENDING_FRAMES {
            state.pending = None;
            return;
        }
        let Some(i) = self.views.iter().position(|v| self.session.get(v.id).and_then(|d| d.path.as_deref()) == Some(pending.path.as_str())) else {
            return; // still opening
        };
        let Some(view) = self.views.get_mut(i) else { return };
        if !pending.started {
            pending.started = true;
            self.active = Some(i);
            let find = view.find.get_or_insert_with(Default::default);
            find.query = pending.query.clone();
            find.case_sensitive = pending.case_sensitive;
            find.whole_words = pending.whole_words;
            find.focus = false;
            view.current = pending.page;
            view.goto = Some((pending.page, 0.0));
            view.rerun_find();
        }
        if !view.texts.contains_key(&pending.page) {
            return;
        }
        // The page's text is in: find the match the result named. The search used the same
        // rules as the find, so its ordinal on the page is the find's too.
        view.rerun_find();
        let target = view
            .find
            .as_ref()
            .and_then(|f| f.matches.iter().enumerate().filter(|(_, (p, _))| *p == pending.page).nth(pending.ordinal).map(|(k, _)| k));
        if let Some(k) = target {
            view.go_to_match(k);
        }
        state.pending = None;
    }
}

/// Draws the dialog when it is open. Called every frame.
pub(crate) fn dialog(app: &mut PdfCraftApp, ctx: &egui::Context) {
    app.search_files_tick();
    if !app.search_files.as_ref().is_some_and(|s| s.open) {
        return;
    }
    let t = Tokens::get(ctx);
    let mut close = false;
    let mut open_result: Option<usize> = None;
    #[cfg(not(target_arch = "wasm32"))]
    let mut pick: Option<bool> = None; // Some(true): a folder
    let modal = widgets::show_modal(ctx, "search-files", |ui| {
        let Some(state) = app.search_files.as_mut() else { return };
        state.refresh();
        ui.set_width(620.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("file-search", 22.0, t.accent));
            ui.label(egui::RichText::new(tl!("Search in multiple PDFs")).font(theme::semibold(16.0)));
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            #[cfg(not(target_arch = "wasm32"))]
            {
                if widgets::pill_button(ui, tl!("Add PDFs…"), false).clicked() {
                    pick = Some(false);
                }
                if widgets::pill_button(ui, tl!("Add a folder…"), false).clicked() {
                    pick = Some(true);
                }
            }
            if !state.entries.is_empty() && widgets::pill_button(ui, tl!("Clear"), false).clicked() {
                state.clear();
            }
        });
        ui.add_space(4.0);
        let reading = state.reading();
        let files = state.entries.len();
        let status = if files == 0 {
            tl!("Add PDFs or a folder to search them.").to_string()
        } else if reading > 0 {
            crate::i18n::fmt(tl!("{n} files · reading {r}…"), &[("n", &files.to_string()), ("r", &reading.to_string())])
        } else {
            crate::i18n::fmt(tl!("{n} files"), &[("n", &files.to_string())])
        };
        ui.horizontal(|ui| {
            if reading > 0 {
                ui.spinner();
            }
            ui.label(egui::RichText::new(status).color(t.text_muted));
        });
        // Files that can't be searched say why: unreadable, or no text at all (a scan).
        for entry in &state.entries {
            let note = match &entry.state {
                State::Failed(e) => Some(crate::i18n::fmt(tl!("{name}: couldn't read it ({e})"), &[("name", &entry.name), ("e", e)])),
                State::Ready(text) if text.has_no_text() => {
                    Some(crate::i18n::fmt(tl!("{name} has no text to search. Recognize text (OCR) in it first."), &[("name", &entry.name)]))
                }
                _ => None,
            };
            if let Some(note) = note {
                ui.label(egui::RichText::new(note).small().color(t.text_faint));
            }
        }
        ui.add_space(8.0);
        let l = ui.label(egui::RichText::new(tl!("What word or phrase would you like to search for?")).color(t.text_muted));
        let r = ui
            .add(widgets::line(&mut state.query).id(egui::Id::new("search-files-input")).hint_text(tl!("Search")).desired_width(f32::INFINITY))
            .labelled_by(l.id);
        if state.focus {
            r.request_focus();
            state.focus = false;
        }
        ui.horizontal(|ui| {
            ui.add(widgets::checkbox(&mut state.whole_words, tl!("Whole words only")));
            ui.add(widgets::checkbox(&mut state.case_sensitive, tl!("Case-sensitive")));
        });
        state.refresh();
        ui.add_space(6.0);
        if !state.query.trim().is_empty() && !state.entries.is_empty() {
            let n = state.results.len();
            let in_files = {
                let mut seen: Vec<usize> = state.results.iter().map(|(i, _)| *i).collect();
                seen.dedup();
                seen.len()
            };
            let count =
                if n == 1 { crate::i18n::fmt(tl!("1 instance"), &[]) } else { crate::i18n::fmt(tl!("{n} instances"), &[("n", &n.to_string())]) };
            let mut line = crate::i18n::fmt(tl!("{count} in {files} files"), &[("count", &count), ("files", &in_files.to_string())]);
            if state.more {
                line.push_str("  ·  ");
                line.push_str(&crate::i18n::fmt(tl!("Only the first {n} are listed."), &[("n", &MAX_RESULTS.to_string())]));
            }
            ui.label(egui::RichText::new(line).font(theme::semibold(12.5)));
        }
        egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut last: Option<(usize, usize)> = None;
            for (k, (file, hit)) in state.results.iter().enumerate() {
                if last.map(|(f, _)| f) != Some(*file) {
                    ui.add_space(6.0);
                    let name = state.entries.get(*file).map(|e| e.name.as_str()).unwrap_or_default();
                    ui.label(egui::RichText::new(name).font(theme::semibold(13.0)).color(t.text));
                }
                if last != Some((*file, hit.page)) {
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt(tl!("Page {label}"), &[("label", &(hit.page + 1).to_string())]))
                            .small()
                            .color(t.text_faint),
                    );
                }
                last = Some((*file, hit.page));
                let mut job = LayoutJob::default();
                let font = FontId::new(12.5, egui::FontFamily::Proportional);
                let plain = TextFormat { font_id: font, color: t.text_muted, ..Default::default() };
                job.append(&hit.before, 0.0, plain.clone());
                job.append(
                    &hit.text,
                    0.0,
                    TextFormat {
                        font_id: theme::semibold(12.5),
                        color: t.text,
                        background: Color32::from_rgba_unmultiplied(255, 214, 0, 90),
                        ..Default::default()
                    },
                );
                job.append(&hit.after, 0.0, plain);
                job.wrap.max_width = ui.available_width();
                if ui.add(egui::Button::selectable(false, job).wrap()).on_hover_text(tl!("Go to this result")).clicked() {
                    open_result = Some(k);
                }
            }
        });
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Close"), true).clicked() {
                close = true;
            }
        });
    });
    if modal.should_close() {
        close = true;
    }
    if let Some(k) = open_result {
        app.search_files_open_result(k);
        return;
    }
    if close && let Some(state) = app.search_files.as_mut() {
        state.open = false;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(folder) = pick {
        let title = if folder { tl!("Choose a folder of PDFs") } else { tl!("Choose PDFs to search") };
        let dialog = rfd::AsyncFileDialog::new().set_title(title);
        if folder {
            app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, dir| {
                let found = pdfs_in_folder(&dir);
                if found.is_empty() {
                    app.notify_tr("That folder has no PDFs.");
                }
                app.search_files_add(found);
            });
        } else {
            let dialog = dialog.add_filter("PDF", &["pdf", "PDF"]);
            app.ask(crate::pickers::Ask::Files(dialog), None, |app, paths| app.search_files_add(paths));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn folder_walk_finds_pdfs_in_subfolders_only() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-search-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        for f in ["a.pdf", "notes.txt", "sub/B.PDF", "sub/deeper/c.pdf"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let found: Vec<String> = pdfs_in_folder(&dir).iter().map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(found, ["a.pdf", "sub/B.PDF", "sub/deeper/c.pdf"]);
        assert!(pdfs_in_folder(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_lists_matches_in_file_order_and_caps_them() {
        let mut s = SearchFiles::default();
        let text = |pages: &[&str]| State::Ready(Arc::new(FileText { pages: pages.iter().map(|p| Ok(p.to_string())).collect(), truncated: false }));
        s.entries.push(Entry { path: "b.pdf".into(), name: "b.pdf".into(), state: text(&["x drug x", "drug"]) });
        s.entries.push(Entry { path: "a.pdf".into(), name: "a.pdf".into(), state: text(&["no"]) });
        s.entries.push(Entry { path: "c.pdf".into(), name: "c.pdf".into(), state: State::Reading });
        s.query = "drug".into();
        s.refresh();
        assert_eq!(s.results.iter().map(|(f, h)| (*f, h.page)).collect::<Vec<_>>(), [(0, 0), (0, 1)]);
        assert!(!s.more);
        s.entries[2].state = text(&[&"drug ".repeat(MAX_RESULTS + 5)]);
        s.key = None;
        s.refresh();
        assert_eq!(s.results.len(), MAX_RESULTS);
        assert!(s.more);
        s.query = "  ".into();
        s.refresh();
        assert!(s.results.is_empty());
    }
}
