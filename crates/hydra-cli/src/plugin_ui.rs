//! Typed plugin forms and management events for the terminal UI.
use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex};

use hya_plugin_api::{Answers, ErrorCode, FieldKind, Form, PluginError, Value};

static HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SENDER: Mutex<Option<mpsc::Sender<Event>>> = Mutex::new(None);

pub enum Event {
    Prompt(Prompt),
    Log(String),
    Installed(Result<Vec<hya_plugin::manager::Installed>, String>),
}
pub struct Prompt {
    pub plugin: String,
    pub form: Form,
    pub values: BTreeMap<String, String>,
    pub selected: usize,
    pub error: Option<String>,
    pub reply: mpsc::SyncSender<Result<Answers, PluginError>>,
}
impl Prompt {
    pub fn submit(&self) -> Result<Answers, PluginError> {
        let mut answers = Answers::new();
        for field in &self.form.fields {
            let value = match self.values.get(&field.key).filter(|s| !s.is_empty()) {
                None => field.default.clone().unwrap_or(Value::Text(String::new())),
                Some(raw) => match field.kind {
                    FieldKind::Bool | FieldKind::Checkbox => Value::Bool(match raw.as_str() {
                        "true" | "yes" | "y" => true,
                        "false" | "no" | "n" => false,
                        _ => {
                            return Err(PluginError::new(
                                ErrorCode::InvalidInput,
                                "enter true or false",
                            ))
                        }
                    }),
                    FieldKind::Number => Value::Number(raw.parse().map_err(|_| {
                        PluginError::new(ErrorCode::InvalidInput, "enter a number")
                    })?),
                    _ => Value::Text(raw.clone()),
                },
            };
            field
                .validate(&value)
                .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e))?;
            answers.insert(field.key.clone(), value);
        }
        Ok(answers)
    }
    pub fn render(&self) -> String {
        let mut output = format!(
            "Plugin {} — {}\r\n\r\n",
            clean(&self.plugin),
            clean(self.form.title.as_deref().unwrap_or("Input required"))
        );
        for (index, field) in self.form.fields.iter().enumerate() {
            let raw = self.values.get(&field.key).cloned().unwrap_or_default();
            let value = if field.kind == FieldKind::Secret {
                "*".repeat(raw.chars().count())
            } else {
                clean(&raw)
            };
            output.push_str(&format!(
                "{} {}: {}\r\n",
                if index == self.selected { ">" } else { " " },
                clean(&field.label),
                value
            ));
            if !field.options.is_empty() {
                output.push_str(&format!("    {}\r\n", clean(&field.options.join(" | "))));
            }
            if let Some(help) = &field.help {
                output.push_str(&format!("    {}\r\n", clean(help)));
            }
        }
        if let Some(error) = &self.error {
            output.push_str(&format!("\r\n{}\r\n", clean(error)));
        }
        output.push_str("\r\nTab/up/down field · Enter submit · Esc cancel\r\n");
        output
    }
}
/// Terminal text is data; plugin labels must not inject control sequences.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}
pub struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        *SENDER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
pub fn attach() -> (Guard, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    *SENDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    (Guard, rx)
}
pub fn active() -> bool {
    SENDER.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}
pub fn send(event: Event) {
    if let Some(sender) = SENDER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = sender.send(event);
    }
}
pub fn prompt(
    plugin: &str,
    form: Form,
    ctl: &hya_plugin::runtime::CallCtl,
) -> Result<Answers, PluginError> {
    let (tx, rx) = mpsc::sync_channel(1);
    send(Event::Prompt(Prompt {
        plugin: plugin.into(),
        form,
        values: Default::default(),
        selected: 0,
        error: None,
        reply: tx,
    }));
    loop {
        ctl.check()?;
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => {
                return Err(PluginError::new(
                    ErrorCode::Cancelled,
                    "terminal prompt closed",
                ))
            }
        }
    }
}

pub struct Headless;
impl Headless {
    pub fn enter() -> Self {
        HEADLESS.store(true, std::sync::atomic::Ordering::Relaxed);
        Self
    }
}
impl Drop for Headless {
    fn drop(&mut self) {
        HEADLESS.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
pub fn headless() -> bool {
    HEADLESS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_validates_types_masks_secrets_and_strips_escape_sequences() {
        let form: Form = serde_json::from_value(serde_json::json!({"fields":[
            {"key":"token","label":"Token","type":"secret"},
            {"key":"enabled","label":"Enabled","type":"bool"},
            {"key":"count","label":"Count","type":"number"}
        ]}))
        .unwrap();
        let (reply, _) = mpsc::sync_channel(1);
        let mut prompt = Prompt {
            plugin: "example.test".into(),
            form,
            values: [
                ("token".into(), "private".into()),
                ("enabled".into(), "true".into()),
                ("count".into(), "2".into()),
            ]
            .into_iter()
            .collect(),
            selected: 0,
            error: None,
            reply,
        };
        assert_eq!(prompt.submit().unwrap()["enabled"], Value::Bool(true));
        assert!(!prompt.render().contains("private"));
        prompt.values.insert("count".into(), "bad".into());
        assert!(prompt.submit().is_err());
        assert!(!clean("\x1b[2J\n").contains('\x1b'));
    }
    #[test]
    fn terminal_prompt_roundtrips_and_cancellation_releases_worker() {
        let (guard, events) = attach();
        assert!(active());
        let ctl = hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(5));
        let form: Form = serde_json::from_value(serde_json::json!({"fields":[]})).unwrap();
        let worker_ctl = ctl.clone();
        let worker_form = form.clone();
        let worker = std::thread::spawn(move || prompt("example.test", worker_form, &worker_ctl));
        let Event::Prompt(request) = events.recv().unwrap() else {
            panic!("prompt")
        };
        request.reply.send(Ok(Answers::new())).unwrap();
        assert!(worker.join().unwrap().is_ok());
        let worker_ctl = ctl.clone();
        let worker = std::thread::spawn(move || prompt("example.test", form, &worker_ctl));
        let Event::Prompt(_request) = events.recv().unwrap() else {
            panic!("prompt")
        };
        ctl.cancel();
        assert_eq!(
            worker.join().unwrap().unwrap_err().code,
            ErrorCode::Cancelled
        );
        drop(guard);
        assert!(!active());
    }
}
