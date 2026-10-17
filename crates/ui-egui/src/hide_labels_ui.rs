//! Auto hide labels: covers every label on a page (or every page) with a white box in one click,
//! so a labelled diagram becomes a self-test. The page's own text is found at once; words inside
//! pictures are read with text recognition when its models are installed. Runs in the background
//! like Recognize text, and lands as one undoable step. The hand-drawn Hide label tool is
//! separate (`comment.hidelabel`).

use std::sync::{Arc, Mutex};

use pdfcraft_engine::ocr::OcrSettings;
use pdfcraft_engine::ocr::hide_labels::{self, PageLabels};

use crate::PdfCraftApp;

/// A run in progress.
pub struct HideRun {
    doc: pdfcraft_engine::DocId,
    progress: Arc<Mutex<HideProgress>>,
}

#[derive(Default)]
struct HideProgress {
    done: usize,
    total: usize,
    /// The labels found, and whether pictures were read too.
    result: Option<(Vec<PageLabels>, bool)>,
}

impl PdfCraftApp {
    /// Hide the labels on the current page (`all = false`) or on every page.
    pub fn auto_hide_labels(&mut self, all: bool) {
        let Some((vi, id)) = self.active_ids() else { return };
        if self.hide_run.is_some() {
            self.notify_tr("Labels are already being hidden");
            return;
        }
        if let Some(why) = self.session.get(id).and_then(|d| d.read_only_reason.clone()) {
            self.notify(why);
            return;
        }
        let pages = if all { Vec::new() } else { vec![self.views[vi].current] };
        let settings = OcrSettings { skip_text_pages: false, ..Default::default() };
        let Some(job) = self.session.ocr_job(id, &pages, settings) else { return };
        let progress = Arc::new(Mutex::new(HideProgress { total: job.pages.len(), ..Default::default() }));
        let p = progress.clone();
        let work = move || {
            // Without the models only the page's own text is found; that is still most labels.
            let ocr = pdfcraft_engine::ocr::engine().ok();
            let found = pdfcraft_engine::guard(|| {
                job.find_labels(ocr.as_deref(), |done, total| {
                    if let Ok(mut s) = p.lock() {
                        s.done = done;
                        s.total = total;
                    }
                    true
                })
            })
            .unwrap_or_default();
            if let Ok(mut s) = p.lock() {
                s.result = Some((found, ocr.is_some()));
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else {
            std::thread::Builder::new().name("pdfcraft-hide-labels".into()).spawn(work).ok();
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.hide_run = Some(HideRun { doc: id, progress });
        self.poll_hide_labels();
    }

    /// Show progress; add the boxes once the worker is done.
    pub(crate) fn poll_hide_labels(&mut self) {
        let Some(run) = self.hide_run.as_ref() else { return };
        let (doc, progress) = (run.doc, run.progress.clone());
        let Ok(mut s) = progress.lock() else {
            self.hide_run = None;
            return;
        };
        let Some((found, read_pictures)) = s.result.take() else {
            let msg = crate::i18n::fmt(
                tl!("Hiding labels… page {d} of {t}"),
                &[("d", &(s.done + 1).min(s.total.max(1)).to_string()), ("t", &s.total.max(1).to_string())],
            );
            drop(s);
            if self.toast.as_ref().is_none_or(|t| t.0 != msg) {
                self.notify(msg);
            }
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            return;
        };
        drop(s);
        self.hide_run = None;
        // The document may have been closed or switched while the worker ran.
        if self.active_ids().map(|(_, id)| id) != Some(doc) {
            return;
        }
        let count: usize = found.iter().map(|p| p.boxes.len()).sum();
        let Some(edit) = hide_labels::edit(&found, &self.comment_prefs.author) else {
            self.notify_tr("No labels found on this page");
            return;
        };
        if self.apply_edit(edit) {
            let msg = if read_pictures {
                tl!("Hid {n} labels. Hide all comments shows them again.")
            } else {
                tl!("Hid {n} labels (labels inside pictures need text recognition, which isn't installed).")
            };
            self.notify(crate::i18n::fmt(msg, &[("n", &count.to_string())]));
        }
    }
}
