//! The AI assistant in the real shell (egui_kittest), with a stand-in model: an answer can be
//! followed up, and the follow-up is sent with the conversation so far.

use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::Session;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::ai::{AiBackend, Ask, Task};

/// Answers "Answer 1", "Answer 2", … and keeps every question it was asked.
#[derive(Default)]
struct Model {
    asked: Mutex<Vec<Ask>>,
}

impl AiBackend for Model {
    fn models(&self) -> Result<Vec<String>, String> {
        Ok(vec![pdfcraft_ui_egui::ai::DEFAULT_MODEL.to_owned()])
    }

    fn pull(&self, _: &str, _: &mut dyn FnMut(f32) -> bool) -> Result<(), String> {
        Ok(())
    }

    fn ask(&self, ask: &Ask, text: &mut dyn FnMut(&str) -> bool) -> Result<(), String> {
        let mut asked = self.asked.lock().map_err(|e| e.to_string())?;
        asked.push(ask.clone());
        text(&format!("Answer {}", asked.len()));
        Ok(())
    }
}

#[test]
fn explain_answers_can_be_followed_up() {
    let model = Arc::new(Model::default());
    let backend = model.clone();
    let pdf = Session::new().create_from_text("t", "The half-life is the time for the plasma level to halve.").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("lecture.pdf", None, pdf).unwrap();
        app.run_inline = true;
        app.ai = Some(backend);
        app
    });
    h.run_steps(4);
    h.state_mut().open_ai(Task::Explain, Some("half-life".into()), Some(0));
    h.run_steps(2);
    h.get_by_label("Answer 1");

    let ask_box = h.get_by_role_and_label(Role::TextInput, "Ask a follow-up question");
    ask_box.focus();
    ask_box.type_text("Why does it matter?");
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Ask").click();
    h.run_steps(3);
    h.get_by_label("You: Why does it matter?");
    h.get_by_label("Answer 2");

    let asked = model.asked.lock().unwrap();
    assert_eq!(asked.len(), 2);
    let (first, follow_up) = (&asked[0], &asked[1]);
    assert!(first.history.is_empty());
    assert!(first.prompt.contains("half-life"));
    assert_eq!(follow_up.history, vec![(first.prompt.clone(), "Answer 1".to_owned())]);
    assert!(follow_up.prompt.contains("Why does it matter?"));
}
