//! The AI assistant: Summarize, Ask about this document, Translate and Explain, answered by a
//! model on this computer (the Mac app runs it with Ollama; see the app's `ollama.rs`). Off
//! unless the desktop app supplies an [`AiBackend`]. Answers stream into one window, are
//! marked as AI answers, and can be copied.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};
use crate::widgets;

/// One question for the model.
#[derive(Clone, Debug)]
pub struct Ask {
    pub model: String,
    pub system: String,
    pub prompt: String,
    /// The conversation so far, oldest first: what was asked, what the model answered.
    pub history: Vec<(String, String)>,
    /// Context window asked for, in tokens.
    pub context: u32,
}

/// What answers: a model run on this computer. Every call blocks; the window runs them on a
/// worker thread.
pub trait AiBackend: Send + Sync {
    /// The models installed.
    fn models(&self) -> Result<Vec<String>, String>;
    /// Download `model`; `progress(0..=1)` returns `false` to stop.
    fn pull(&self, model: &str, progress: &mut dyn FnMut(f32) -> bool) -> Result<(), String>;
    /// Answer `ask`, handing each piece of text to `text` as it comes; `false` stops.
    fn ask(&self, ask: &Ask, text: &mut dyn FnMut(&str) -> bool) -> Result<(), String>;
}

/// The models offered: Ollama name, label.
pub const MODELS: &[(&str, &str)] = &[
    ("gemma3:4b", "Gemma 3 4B — fast, 3.4 GB"),
    ("gemma4:e4b", "Gemma 4 E4B — newer, 6.6 GB"),
    ("deepseek-r1:8b", "DeepSeek-R1 8B — reasons step by step, 5.2 GB"),
];
pub const DEFAULT_MODEL: &str = "gemma3:4b";

/// Most characters of a document sent with a question (about 12,000 tokens).
const MAX_DOCUMENT: usize = 48_000;
const CONTEXT: u32 = 16_384;
/// The largest context a follow-up asks for: a long conversation about a whole lecture.
const MAX_CONTEXT: u32 = 32_768;

const SYSTEM: &str = "You are a careful study assistant for a pharmacy student. Base your answers on the lecture text you are given. \
Be concise and accurate; use short bullet points where they help. If the lecture does not say, say so plainly, and mark anything you add from general knowledge as such. \
Never invent doses or facts.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Summarize,
    Ask,
    Translate,
    Explain,
}

impl Task {
    fn title(self) -> &'static str {
        match self {
            Task::Summarize => "Summarize",
            Task::Ask => "Ask about this document",
            Task::Translate => "Translate",
            Task::Explain => "Explain with AI",
        }
    }
}

#[derive(Default)]
struct Job {
    /// What was sent to the model (kept for follow-up questions).
    prompt: String,
    text: String,
    status: String,
    /// Download progress, while a model downloads.
    progress: Option<f32>,
    /// The model isn't installed: offer to download it.
    missing_model: bool,
    error: Option<String>,
    done: bool,
    stop: bool,
}

/// The open window.
pub struct AiWindow {
    doc: pdfcraft_engine::DocId,
    task: Task,
    /// The selection (Translate, Explain) or the question (Ask).
    input: String,
    /// The page the selection is on (Explain): its text is the context.
    page: Option<usize>,
    /// Answered questions, oldest first; the running or last job is not among them.
    turns: Vec<Turn>,
    /// The follow-up question the job answers (`None` for the first question).
    asking: Option<String>,
    /// The follow-up being typed.
    follow_up: String,
    job: Option<Arc<Mutex<Job>>>,
}

/// One answered question in the conversation.
struct Turn {
    /// The follow-up asked (`None` for the first question, shown above the conversation).
    question: Option<String>,
    prompt: String,
    answer: String,
}

/// A follow-up question, answered from what was said before.
fn follow_up_prompt(question: &str) -> String {
    format!("Follow-up question about the text above: {question}")
}

/// The context to ask for: room for the whole conversation (about 3 characters a token) and
/// the answer, between [`CONTEXT`] and [`MAX_CONTEXT`].
fn context_for(history: &[(String, String)], prompt: &str) -> u32 {
    let chars = history.iter().map(|(q, a)| q.len().saturating_add(a.len())).fold(prompt.len(), usize::saturating_add);
    u32::try_from(chars / 3).unwrap_or(u32::MAX).saturating_add(4_096).clamp(CONTEXT, MAX_CONTEXT)
}

fn lang_name(code: &str) -> &'static str {
    pdfcraft_engine::dictionary::LANGUAGES.iter().find(|(c, _)| *c == code).map_or("Arabic", |(_, n)| n)
}

/// The prompt for `task`, with the document's text (cut to fit) where the task needs it.
fn prompt(task: Task, input: &str, document: &str, language: &str) -> String {
    let doc: String = document.chars().take(MAX_DOCUMENT).collect();
    match task {
        Task::Summarize => format!(
            "Summarize this lecture for revision. Give: the main topics; the key points under each as short bullets; important definitions; any formulas; drugs or examples mentioned.\n\nLECTURE:\n{doc}"
        ),
        Task::Ask => format!("Answer the question using the lecture below.\n\nQUESTION: {input}\n\nLECTURE:\n{doc}"),
        Task::Translate => format!(
            "Translate the following text into {}. Keep scientific and drug names accurate (keep the English term in brackets after a technical word). Reply with the translation only.\n\nTEXT:\n{input}",
            lang_name(language)
        ),
        Task::Explain => format!(
            "Explain in simple words, in 3 to 5 sentences, what this means in the context of the lecture page below: \"{input}\"\n\nLECTURE PAGE:\n{doc}"
        ),
    }
}

impl PdfCraftApp {
    /// Open the assistant on `task`. `input` is the selected text (Translate, Explain).
    pub fn open_ai(&mut self, task: Task, input: Option<String>, page: Option<usize>) {
        let Some((_, id)) = self.active_ids() else { return };
        if self.ai.is_none() {
            self.notify_tr("The AI assistant runs in the Mac app with Ollama.");
            return;
        }
        let input = input.unwrap_or_default();
        if matches!(task, Task::Translate | Task::Explain) && input.trim().is_empty() {
            self.notify_tr("Select the text first, then choose this again.");
            return;
        }
        self.ai_window = Some(AiWindow { doc: id, task, input, page, turns: Vec::new(), asking: None, follow_up: String::new(), job: None });
        if task != Task::Ask {
            self.start_ai();
        }
    }

    /// Run the open window's task (or download its model first when `download`).
    fn start_ai_with(&mut self, download: bool) {
        let Some(backend) = self.ai.clone() else { return };
        let Some(w) = self.ai_window.as_mut() else { return };
        let Some(doc) = self.session.get(w.doc) else { return };
        let bytes = doc.bytes.clone();
        let password = doc.password.as_deref().map(Arc::from);
        let (task, input, page) = (w.task, w.input.clone(), w.page);
        // A follow-up: the conversation so far carries the lecture; don't read it again.
        let follow_up =
            w.asking.as_deref().map(|q| (w.turns.iter().map(|t| (t.prompt.clone(), t.answer.clone())).collect::<Vec<_>>(), follow_up_prompt(q)));
        let model = self.ai_model.clone();
        let language = self.lookup_language.clone();
        let job = Arc::new(Mutex::new(Job { status: tl!("Starting…").to_owned(), ..Default::default() }));
        w.job = Some(job.clone());
        let ctx = self.ctx.clone();
        let repaint = move || {
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        let work = move || {
            let set = |f: &mut dyn FnMut(&mut Job)| {
                if let Ok(mut j) = job.lock() {
                    f(&mut j);
                }
                repaint();
            };
            let stopped = || job.lock().map(|j| j.stop).unwrap_or(true);
            let run = || -> Result<(), String> {
                set(&mut |j| j.status = tl!("Starting the AI…").to_owned());
                let installed = backend.models()?;
                let have = installed.iter().any(|m| m == &model || m.strip_suffix(":latest") == Some(model.as_str()));
                if !have && !download {
                    set(&mut |j| j.missing_model = true);
                    return Ok(());
                }
                if !have {
                    set(&mut |j| {
                        j.missing_model = false;
                        j.status = crate::i18n::fmt(tl!("Downloading {model}…"), &[("model", &model)]);
                    });
                    backend.pull(&model, &mut |p| {
                        set(&mut |j| j.progress = Some(p));
                        !stopped()
                    })?;
                    set(&mut |j| j.progress = None);
                }
                if let Some((history, prompt)) = &follow_up {
                    set(&mut |j| {
                        j.prompt = prompt.clone();
                        j.status = tl!("Thinking…").to_owned();
                    });
                    let ask = Ask {
                        model: model.clone(),
                        system: SYSTEM.to_owned(),
                        prompt: prompt.clone(),
                        context: context_for(history, prompt),
                        history: history.clone(),
                    };
                    return backend.ask(&ask, &mut |piece| {
                        set(&mut |j| j.text.push_str(piece));
                        !stopped()
                    });
                }
                set(&mut |j| j.status = tl!("Reading the lecture…").to_owned());
                // The document's text: all of it, or the selection's page (Explain).
                let document = if task == Task::Translate {
                    String::new()
                } else {
                    let text = pdfcraft_render::search_files::extract_file_text(bytes, password, &stopped)?;
                    let pages: Vec<(usize, String)> = text
                        .pages
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, p)| p.ok().map(|t| (i, t)))
                        .filter(|(i, _)| page.is_none_or(|p| p == *i))
                        .collect();
                    if pages.iter().all(|(_, t)| t.trim().is_empty()) && task != Task::Explain {
                        return Err(tl!("This PDF has no text to read. Recognize text (Scan & OCR) first.").to_owned());
                    }
                    pages.into_iter().map(|(i, t)| format!("[Page {}]\n{t}", i + 1)).collect::<Vec<_>>().join("\n\n")
                };
                let prompt = prompt(task, &input, &document, &language);
                set(&mut |j| {
                    j.prompt = prompt.clone();
                    j.status = tl!("Thinking…").to_owned();
                });
                let ask = Ask { model: model.clone(), system: SYSTEM.to_owned(), prompt, history: Vec::new(), context: CONTEXT };
                backend.ask(&ask, &mut |piece| {
                    set(&mut |j| j.text.push_str(piece));
                    !stopped()
                })
            };
            let result = pdfcraft_engine::guard(run).unwrap_or_else(Err);
            set(&mut |j| {
                if let Err(e) = &result {
                    j.error = Some(e.clone());
                }
                j.done = true;
            });
        };
        if self.run_inline {
            work();
        } else {
            std::thread::Builder::new().name("pdfcraft-ai".into()).spawn(work).ok();
        }
    }

    fn start_ai(&mut self) {
        self.start_ai_with(false);
    }

    /// Ask the typed follow-up: the finished answer joins the conversation, then the model
    /// answers the new question with everything said so far.
    fn ask_follow_up(&mut self) {
        let Some(w) = self.ai_window.as_mut() else { return };
        let question = w.follow_up.trim().to_owned();
        let finished = w.job.as_ref().and_then(|j| {
            let j = j.lock().ok()?;
            let answer = visible_answer(&j.text).0.trim();
            (j.done && j.error.is_none() && !j.prompt.is_empty() && !answer.is_empty()).then(|| (j.prompt.clone(), answer.to_owned()))
        });
        let Some((prompt, answer)) = finished else { return };
        if question.is_empty() {
            return;
        }
        w.turns.push(Turn { question: w.asking.take(), prompt, answer });
        w.asking = Some(question);
        w.follow_up.clear();
        self.start_ai();
    }
}

/// DeepSeek-R1 shows its reasoning between `<think>` tags: the answer is what follows.
fn visible_answer(text: &str) -> (&str, bool) {
    match (text.find("<think>"), text.find("</think>")) {
        (Some(_), Some(end)) => (text.get(end + "</think>".len()..).unwrap_or("").trim_start(), false),
        (Some(_), None) => ("", true),
        _ => (text, false),
    }
}

pub fn window(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(w) = app.ai_window.as_ref() else { return };
    if app.active_ids().map(|(_, id)| id) != Some(w.doc) {
        // Switching documents ends the conversation about the old one.
        if let Some(j) = &w.job
            && let Ok(mut j) = j.lock()
        {
            j.stop = true;
        }
        app.ai_window = None;
        return;
    }
    let t = Tokens::get(ctx);
    let mut open = true;
    let mut start = false;
    let mut download = false;
    let mut stop = false;
    let mut follow_up = false;
    let mut copy = None;
    let mut model = None;
    let task = w.task;
    let current_model = app.ai_model.clone();
    let Some(w) = app.ai_window.as_mut() else { return };
    let job =
        w.job.as_ref().and_then(|j| j.lock().ok().map(|j| (j.text.clone(), j.status.clone(), j.progress, j.missing_model, j.error.clone(), j.done)));
    egui::Window::new(tl!(task.title()))
        .id(egui::Id::new("ai-assistant"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([480.0, 460.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Model")).color(t.text_muted));
                let label = MODELS.iter().find(|(m, _)| *m == current_model).map_or(current_model.as_str(), |(_, l)| l);
                egui::ComboBox::from_id_salt("ai-model").selected_text(label).width(300.0).show_ui(ui, |ui| {
                    for (m, l) in MODELS {
                        if ui.selectable_label(current_model == *m, *l).clicked() {
                            model = Some((*m).to_owned());
                        }
                    }
                });
            });
            if task == Task::Ask && job.is_none() {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut w.input).hint_text(tl!("Ask a question about this lecture")).desired_width(f32::INFINITY),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !w.input.trim().is_empty() {
                        start = true;
                    }
                });
                if ui.add_enabled(!w.input.trim().is_empty(), egui::Button::new(tl!("Ask"))).clicked() {
                    start = true;
                }
            } else if !w.input.is_empty() {
                let shown: String = w.input.chars().take(160).collect();
                ui.label(
                    egui::RichText::new(format!("“{shown}{}”", if w.input.chars().count() > 160 { "…" } else { "" })).italics().color(t.text_muted),
                );
            }
            ui.separator();
            match &job {
                None => {
                    if task == Task::Ask {
                        ui.label(egui::RichText::new(tl!("The answer comes from the lecture's text, on this Mac.")).color(t.text_muted));
                    }
                }
                Some((text, status, progress, missing, error, done)) => {
                    let (answer, thinking) = visible_answer(text);
                    if !w.turns.is_empty() || w.asking.is_some() || !answer.is_empty() {
                        let question = |ui: &mut egui::Ui, q: &str| {
                            ui.label(egui::RichText::new(format!("{}: {q}", tl!("You"))).strong().color(t.text_muted));
                        };
                        egui::ScrollArea::vertical().max_height(340.0).stick_to_bottom(true).show(ui, |ui| {
                            for turn in &w.turns {
                                if let Some(q) = &turn.question {
                                    question(ui, q);
                                }
                                ui.label(egui::RichText::new(&turn.answer).font(theme::regular(13.5)));
                                ui.add_space(10.0);
                            }
                            if let Some(q) = &w.asking {
                                question(ui, q);
                            }
                            if !answer.is_empty() {
                                ui.label(egui::RichText::new(answer).font(theme::regular(13.5)));
                            }
                        });
                    }
                    if *missing {
                        let size = MODELS.iter().find(|(m, _)| *m == current_model).map_or("", |(_, l)| l);
                        ui.label(crate::i18n::fmt(
                            tl!("{model} isn't downloaded yet ({size}). It's downloaded once, then works offline."),
                            &[("model", &current_model), ("size", size)],
                        ));
                        download = ui.button(tl!("Download the model")).clicked();
                    } else if let Some(e) = error {
                        ui.label(egui::RichText::new(e).color(t.text_muted));
                        if e.contains("ollama.com") && ui.link(tl!("Get Ollama (free)")).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab("https://ollama.com/download/mac"));
                        }
                        start = ui.button(tl!("Try again")).clicked();
                    }
                    if let Some(p) = progress {
                        ui.add(egui::ProgressBar::new(*p).show_percentage().text(status.clone()));
                    }
                    if !*done && (answer.is_empty() || thinking) && progress.is_none() && !*missing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(if thinking { tl!("Thinking…").to_owned() } else { status.clone() });
                        });
                    }
                    if *done && error.is_none() && !*missing && !answer.trim().is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let button_width = 56.0;
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut w.follow_up)
                                    .hint_text(tl!("Ask a follow-up question"))
                                    .desired_width((ui.available_width() - button_width).max(80.0)),
                            );
                            // Screen readers name the box by its hint.
                            let value = w.follow_up.clone();
                            r.widget_info(|| egui::WidgetInfo {
                                label: Some(tl!("Ask a follow-up question").to_owned()),
                                ..egui::WidgetInfo::text_edit(true, value.clone(), value.clone(), "")
                            });
                            let ready = !w.follow_up.trim().is_empty();
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                                follow_up = true;
                            }
                            if ui.add_enabled(ready, egui::Button::new(tl!("Ask"))).clicked() {
                                follow_up = true;
                            }
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if !*done {
                            stop = widgets::ghost_button(ui, "square", tl!("Stop")).clicked();
                        } else if !answer.is_empty() && widgets::ghost_button(ui, "copy", tl!("Copy")).clicked() {
                            copy = Some(answer.to_owned());
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(egui::RichText::new(tl!("AI answer · check important facts")).small().color(t.text_faint));
                        });
                    });
                }
            }
        });
    if let Some(m) = model {
        app.ai_model = m;
    }
    if let Some(text) = copy {
        ctx.copy_text(text);
    }
    if stop
        && let Some(j) = app.ai_window.as_ref().and_then(|w| w.job.as_ref())
        && let Ok(mut j) = j.lock()
    {
        j.stop = true;
    }
    if start || download {
        app.start_ai_with(download);
    } else if follow_up {
        app.ask_follow_up();
    }
    if !open {
        if let Some(j) = app.ai_window.as_ref().and_then(|w| w.job.as_ref())
            && let Ok(mut j) = j.lock()
        {
            j.stop = true;
        }
        app.ai_window = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_is_hidden_until_the_answer() {
        assert_eq!(visible_answer("<think>let me see"), ("", true));
        assert_eq!(visible_answer("<think>x</think>\n\nThe answer"), ("The answer", false));
        assert_eq!(visible_answer("Plain answer"), ("Plain answer", false));
    }

    #[test]
    fn prompts_carry_the_lecture_cut_to_fit() {
        let long = "x".repeat(MAX_DOCUMENT * 2);
        let p = prompt(Task::Summarize, "", &long, "ar");
        assert!(p.len() < MAX_DOCUMENT + 400);
        assert!(prompt(Task::Translate, "half-life", "", "de").contains("German"));
        assert!(prompt(Task::Ask, "What is Vd?", "lecture", "ar").contains("What is Vd?"));
    }

    #[test]
    fn follow_ups_ask_for_room_for_the_conversation() {
        assert_eq!(context_for(&[], "short"), CONTEXT);
        let lecture = ("x".repeat(MAX_DOCUMENT), "an answer".to_owned());
        let c = context_for(std::slice::from_ref(&lecture), &follow_up_prompt("and then?"));
        assert!(c > CONTEXT && c <= MAX_CONTEXT, "{c}");
        let long = vec![lecture; 50];
        assert_eq!(context_for(&long, "why?"), MAX_CONTEXT);
    }
}
