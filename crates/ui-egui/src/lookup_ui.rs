//! Define and Translate on selected text: a small card next to the selection with what the
//! offline dictionaries say ([`pdfcraft_engine::dictionary`]). The dictionaries load on a worker
//! thread the first time (about a second); every lookup after that is instant.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout, Pos2};
use pdfcraft_engine::dictionary::{self, Dictionary, Entry, LANGUAGES};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};
use crate::widgets;

type Loading = Arc<Mutex<Option<Result<Arc<Dictionary>, String>>>>;

/// The open card.
pub struct LookupCard {
    query: String,
    at: Pos2,
    translate: bool,
    entries: Option<Vec<Entry>>,
}

/// The dictionaries, once loaded, or the load under way.
#[derive(Default)]
pub struct Dictionaries {
    loaded: Option<Result<Arc<Dictionary>, String>>,
    loading: Option<Loading>,
}

impl PdfCraftApp {
    /// Look up `text` (selected on the page, shown at `at`): its definitions, or with
    /// `translate` its translation.
    pub fn open_lookup(&mut self, text: String, at: Pos2, translate: bool) {
        if dictionary::fold(&text).is_empty() {
            return;
        }
        self.lookup = Some(LookupCard { query: text, at, translate, entries: None });
        self.poll_dictionary();
    }

    /// Start loading the dictionaries if needed; fill the card once they are there.
    fn poll_dictionary(&mut self) {
        let d = &mut self.dictionaries;
        if d.loaded.is_none() {
            match &d.loading {
                None => {
                    let slot: Loading = Arc::default();
                    let s = slot.clone();
                    let ctx = self.ctx.clone();
                    let work = move || {
                        let r = pdfcraft_engine::guard(dictionary::shared).unwrap_or_else(Err);
                        if let Ok(mut s) = s.lock() {
                            *s = Some(r);
                        }
                        if let Some(ctx) = ctx {
                            ctx.request_repaint();
                        }
                    };
                    if self.run_inline {
                        work();
                    } else if std::thread::Builder::new().name("pdfcraft-dictionary".into()).spawn(work).is_err() {
                        d.loaded = Some(Err("couldn't start loading the dictionaries".into()));
                    }
                    if d.loaded.is_none() {
                        d.loading = Some(slot);
                    }
                }
                Some(slot) => {
                    let done = slot.lock().ok().and_then(|mut s| s.take());
                    if let Some(r) = done {
                        d.loaded = Some(r);
                        d.loading = None;
                    }
                }
            }
            // An inline load lands at once.
            if let Some(slot) = &d.loading
                && let Some(r) = slot.lock().ok().and_then(|mut s| s.take())
            {
                d.loaded = Some(r);
                d.loading = None;
            }
        }
        if let (Some(card), Some(Ok(dict))) = (self.lookup.as_mut(), &self.dictionaries.loaded)
            && card.entries.is_none()
        {
            card.entries = Some(dict.lookup(&card.query));
        }
    }
}

fn language_name(code: &str) -> &'static str {
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or("Arabic", |(_, n)| n)
}

/// The card's text, for Copy.
fn as_text(entries: &[Entry], translate: bool, language: &str) -> String {
    let mut s = String::new();
    for e in entries {
        if translate {
            let words: Vec<&str> = e.translations.iter().filter(|t| t.language == language).map(|t| t.words.as_str()).collect();
            s.push_str(&format!("{}: {}\n", e.query, if words.is_empty() { "—".to_owned() } else { words.join("; ") }));
        } else {
            s.push_str(&format!("{}\n", e.query));
            for d in &e.definitions {
                s.push_str(&format!("  ({}) {}\n", if d.part_of_speech.is_empty() { &d.source } else { &d.part_of_speech }, d.text));
            }
        }
    }
    s
}

pub fn card(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.lookup.is_none() {
        return;
    }
    app.poll_dictionary();
    let t = Tokens::get(ctx);
    let language = app.lookup_language.clone();
    let error = match &app.dictionaries.loaded {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    };
    let Some(card) = app.lookup.as_mut() else { return };
    let mut open = true;
    let mut new_language = None;
    let mut copy = None;
    let title: String = card.query.chars().take(40).collect();
    egui::Window::new(title)
        .id(egui::Id::new("dictionary-lookup"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(card.at)
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.horizontal(|ui| {
                if ui.selectable_label(!card.translate, tl!("Define")).clicked() {
                    card.translate = false;
                }
                if ui.selectable_label(card.translate, tl!("Translate")).clicked() {
                    card.translate = true;
                }
                if card.translate {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("dictionary-language").selected_text(tl!(language_name(&language))).show_ui(ui, |ui| {
                            for (code, name) in LANGUAGES {
                                if ui.selectable_label(language == *code, tl!(name)).clicked() {
                                    new_language = Some((*code).to_owned());
                                }
                            }
                        });
                    });
                }
            });
            ui.separator();
            if let Some(e) = &error {
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Dictionaries unavailable: {e}"), &[("e", e)])).color(t.text_muted));
                return;
            }
            let Some(entries) = &card.entries else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Loading dictionaries…"));
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            };
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                if entries.iter().all(Entry::is_empty) {
                    ui.label(egui::RichText::new(tl!("Not in the dictionaries.")).color(t.text_muted));
                }
                let many = entries.len() > 1;
                for e in entries {
                    if card.translate {
                        let rows: Vec<_> = e.translations.iter().filter(|tr| tr.language == language).collect();
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(&e.query).font(theme::semibold(13.5)));
                            ui.label("→");
                            if rows.is_empty() {
                                ui.label(egui::RichText::new("—").color(t.text_muted));
                            }
                        });
                        for tr in rows {
                            ui.label(egui::RichText::new(&tr.words).font(theme::regular(16.0)));
                            let note =
                                [tr.part_of_speech.as_str(), tr.sense.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                            if !note.is_empty() {
                                ui.label(egui::RichText::new(note).small().color(t.text_muted));
                            }
                        }
                    } else {
                        if many || e.found.as_deref().is_some_and(|f| f != dictionary::fold(&e.query)) {
                            let head = match &e.found {
                                Some(f) if *f != dictionary::fold(&e.query) => format!("{} ({f})", e.query),
                                _ => e.query.clone(),
                            };
                            ui.label(egui::RichText::new(head).font(theme::semibold(13.5)));
                        }
                        if e.definitions.is_empty() {
                            ui.label(egui::RichText::new(tl!("No definition found.")).color(t.text_muted));
                        }
                        for d in &e.definitions {
                            let tag = if d.part_of_speech.is_empty() { d.source.clone() } else { format!("{} · {}", d.source, d.part_of_speech) };
                            ui.label(egui::RichText::new(tag).small().color(t.text_muted));
                            ui.label(egui::RichText::new(&d.text).font(theme::regular(13.5)));
                            ui.add_space(4.0);
                        }
                    }
                    if many {
                        ui.add_space(6.0);
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if widgets::ghost_button(ui, "copy", tl!("Copy")).clicked() {
                    copy = Some(as_text(entries, card.translate, &language));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(tl!("Offline · WordNet, Wiktionary, MeSH")).small().color(t.text_faint));
                });
            });
        });
    if let Some(text) = copy {
        ctx.copy_text(text);
    }
    if let Some(code) = new_language {
        app.lookup_language = code;
    }
    if !open {
        app.lookup = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_engine::dictionary::{Definition, Translation};

    #[test]
    fn copied_text_says_what_the_card_shows() {
        let e = Entry {
            query: "absorption".into(),
            found: Some("absorption".into()),
            definitions: vec![Definition {
                term: "Absorption".into(),
                source: "MeSH".into(),
                part_of_speech: "medical".into(),
                text: "Uptake".into(),
            }],
            translations: vec![Translation {
                language: "ar".into(), part_of_speech: "noun".into(), words: "امتصاص".into(), sense: String::new()
            }],
        };
        assert_eq!(as_text(std::slice::from_ref(&e), false, "ar"), "absorption\n  (medical) Uptake\n");
        assert_eq!(as_text(std::slice::from_ref(&e), true, "ar"), "absorption: امتصاص\n");
        assert_eq!(as_text(&[e], true, "de"), "absorption: —\n");
        assert_eq!(language_name("de"), "German");
        assert_eq!(language_name("xx"), "Arabic");
    }
}
