//! Collect highlights: a window listing the text under every highlight in the document, page by
//! page, for revising. Clicking an entry goes to its page; the whole list can be copied or saved
//! as a text file.

use egui::{Align, Layout};
use pdfcraft_engine::highlights::{self, Highlight};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};
use crate::widgets;

/// The open window: the document it was collected from, its name, and the highlights.
pub struct HighlightsWindow {
    doc: pdfcraft_engine::DocId,
    title: String,
    items: Vec<Highlight>,
}

impl PdfCraftApp {
    /// Collect the active document's highlights and show them.
    pub fn collect_highlights(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let items = self.session.highlights(id);
        if items.is_empty() {
            self.notify_tr("This document has no highlights yet. Select text and choose Highlight first.");
            return;
        }
        let title = self.session.get(id).map(|d| d.display_name()).unwrap_or_default();
        self.highlights_window = Some(HighlightsWindow { doc: id, title, items });
    }
}

pub fn window(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(w) = app.highlights_window.as_ref() else { return };
    // Closing or switching the document closes the list that belongs to it.
    if app.active_ids().map(|(_, id)| id) != Some(w.doc) {
        app.highlights_window = None;
        return;
    }
    let t = Tokens::get(ctx);
    let mut open = true;
    let mut go_to = None;
    let mut copy = false;
    let mut save = false;
    egui::Window::new(tl!("Highlights")).open(&mut open).collapsible(false).resizable(true).default_size([460.0, 520.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&w.title).font(theme::semibold(14.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    save = widgets::ghost_button(ui, "save", tl!("Save as text…")).clicked();
                }
                copy = widgets::ghost_button(ui, "copy", tl!("Copy all")).clicked();
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut last = None;
            for h in &w.items {
                if last != Some(h.page) {
                    ui.add_space(6.0);
                    let label = crate::i18n::fmt(tl!("Page {p}"), &[("p", &(h.page + 1).to_string())]);
                    ui.label(egui::RichText::new(label).font(theme::semibold(12.0)).color(t.text_muted));
                    last = Some(h.page);
                }
                let text = if h.text.is_empty() { tl!("(no text: recognize text on this page first)").to_owned() } else { h.text.clone() };
                let resp = ui.add(egui::Label::new(egui::RichText::new(format!("• {text}")).font(theme::regular(13.5))).sense(egui::Sense::click()));
                if let Some(note) = &h.note {
                    ui.label(egui::RichText::new(format!("   {note}")).italics().color(t.text_muted));
                }
                if theme::hand(resp).on_hover_text(tl!("Go to this page")).clicked() {
                    go_to = Some(h.page);
                }
            }
        });
    });
    let text = highlights::as_text(&w.title, &w.items);
    let title = w.title.clone();
    if copy {
        ctx.copy_text(text.clone());
        app.notify_tr("Copied");
    }
    #[cfg(not(target_arch = "wasm32"))]
    if save {
        let stem = std::path::Path::new(&title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Highlights".into());
        let dialog = rfd::AsyncFileDialog::new().set_file_name(format!("{stem} highlights.txt")).add_filter("Text", &["txt"]);
        app.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, path| match std::fs::write(&path, text.as_bytes()) {
            Ok(()) => app.notify(crate::i18n::fmt(tl!("Saved {name}"), &[("name", &path.display().to_string())])),
            Err(e) => app.notify_error(e.to_string()),
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (save, title);
    if let (Some(page), Some((i, _))) = (go_to, app.active_ids())
        && let Some(view) = app.views.get_mut(i)
    {
        view.goto = Some((page, 0.0));
    }
    if !open {
        app.highlights_window = None;
    }
}
