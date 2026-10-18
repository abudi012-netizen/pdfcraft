//! The AI assistant's model, run on this Mac by Ollama (https://ollama.com): PdfCraft talks to
//! it over `http://127.0.0.1:11434` only, so lectures never leave the computer. Ollama keeps
//! the models; PdfCraft only asks it to download one, list them and answer.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use pdfcraft_ui_egui::ai::{AiBackend, Ask};

const BASE: &str = "http://127.0.0.1:11434";
/// Most bytes of one answer read (a runaway model must not fill memory).
const MAX_ANSWER: usize = 512 * 1024;

pub struct Ollama {
    quick: ureq::Agent,
    long: ureq::Agent,
}

impl Ollama {
    pub fn new() -> Self {
        let agent = |secs: Option<u64>| {
            ureq::Agent::config_builder()
                .timeout_global(secs.map(Duration::from_secs))
                .timeout_connect(Some(Duration::from_secs(3)))
                .http_status_as_error(false)
                .build()
                .new_agent()
        };
        // Answers and downloads take as long as they take; listing models is quick.
        Ollama { quick: agent(Some(5)), long: agent(None) }
    }

    fn not_running(e: impl std::fmt::Display) -> String {
        format!("Ollama isn't running ({e}). Install it from ollama.com, open it, then try again.")
    }

    /// POST `body` to `path` and hand each line of the streamed JSON answer to `line`.
    fn stream(&self, path: &str, body: &serde_json::Value, line: &mut dyn FnMut(&serde_json::Value) -> bool) -> Result<(), String> {
        let mut response =
            self.long.post(format!("{BASE}{path}")).header("Content-Type", "application/json").send(body.to_string()).map_err(Self::not_running)?;
        let status = response.status().as_u16();
        let reader = BufReader::new(response.body_mut().as_reader());
        if !(200..300).contains(&status) {
            let text: String = reader.lines().map_while(Result::ok).collect::<Vec<_>>().join(" ");
            let msg = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_owned)).unwrap_or(text);
            return Err(format!("Ollama said: {msg}"));
        }
        for l in reader.lines() {
            let l = l.map_err(|e| format!("the answer stopped ({e})"))?;
            if l.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&l).map_err(|e| format!("unreadable answer ({e})"))?;
            if let Some(e) = v["error"].as_str() {
                return Err(format!("Ollama said: {e}"));
            }
            if !line(&v) || v["done"].as_bool() == Some(true) {
                break;
            }
        }
        Ok(())
    }
}

impl Ollama {
    fn tags(&self) -> Result<String, String> {
        let mut response = self.quick.get(format!("{BASE}/api/tags")).call().map_err(Self::not_running)?;
        response.body_mut().with_config().limit(4 << 20).read_to_string().map_err(Self::not_running)
    }

    /// Start Ollama in the background when it isn't running (the Ollama app, hidden, or the
    /// `ollama` command), and wait up to 20 seconds for it to answer.
    fn start(&self) -> Result<String, String> {
        let started = std::process::Command::new("/usr/bin/open").args(["-g", "-j", "-a", "Ollama"]).status().is_ok_and(|s| s.success())
            || ["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama"].iter().any(|bin| {
                std::path::Path::new(bin).is_file()
                    && std::process::Command::new(bin)
                        .arg("serve")
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .is_ok()
            });
        if !started {
            return Err("Ollama isn't installed. Get it free from ollama.com, then try again.".into());
        }
        let mut last = String::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(500));
            match self.tags() {
                Ok(body) => return Ok(body),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl AiBackend for Ollama {
    fn models(&self) -> Result<Vec<String>, String> {
        // Not running yet: start it, so the AI buttons just work.
        let body = match self.tags() {
            Ok(body) => body,
            Err(_) => self.start()?,
        };
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("unreadable answer from Ollama ({e})"))?;
        Ok(v["models"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str().map(str::to_owned)).collect())
    }

    fn pull(&self, model: &str, progress: &mut dyn FnMut(f32) -> bool) -> Result<(), String> {
        let body = serde_json::json!({ "model": model, "stream": true });
        let mut stopped = false;
        self.stream("/api/pull", &body, &mut |v| {
            if let (Some(done), Some(total)) = (v["completed"].as_f64(), v["total"].as_f64())
                && total > 0.0
            {
                stopped = !progress((done / total).clamp(0.0, 1.0) as f32);
            }
            !stopped && v["status"].as_str() != Some("success")
        })?;
        if stopped { Err("the download was stopped".into()) } else { Ok(()) }
    }

    fn ask(&self, ask: &Ask, text: &mut dyn FnMut(&str) -> bool) -> Result<(), String> {
        let body = serde_json::json!({
            "model": ask.model,
            "stream": true,
            "messages": messages(ask),
            // Room for a whole lecture; Ollama's default context is much smaller.
            "options": { "num_ctx": ask.context, "temperature": 0.3 },
        });
        let mut received = 0usize;
        self.stream("/api/chat", &body, &mut |v| {
            let part = v["message"]["content"].as_str().unwrap_or("");
            received = received.saturating_add(part.len());
            received <= MAX_ANSWER && text(part)
        })
    }
}

/// The chat sent: the instructions, the conversation so far, then the new question.
fn messages(ask: &Ask) -> Vec<serde_json::Value> {
    let mut messages = vec![serde_json::json!({ "role": "system", "content": ask.system })];
    for (question, answer) in &ask.history {
        messages.push(serde_json::json!({ "role": "user", "content": question }));
        messages.push(serde_json::json!({ "role": "assistant", "content": answer }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": ask.prompt }));
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_ups_carry_the_conversation_in_order() {
        let ask = Ask {
            model: "gemma3:4b".into(),
            system: "sys".into(),
            prompt: "why?".into(),
            history: vec![("explain Vd".into(), "Vd is…".into())],
            context: 16_384,
        };
        let roles: Vec<_> =
            messages(&ask).iter().map(|m| format!("{}={}", m["role"].as_str().unwrap_or(""), m["content"].as_str().unwrap_or(""))).collect();
        assert_eq!(roles, ["system=sys", "user=explain Vd", "assistant=Vd is…", "user=why?"]);
    }
}
