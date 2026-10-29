//! The model, on an OpenAI-compatible server: found on this machine or
//! named, asked for its next message with the tools it may call. A model
//! whose server takes no tools (or calls them badly) is asked for its next
//! action as JSON instead, and the chat checks it before running anything.

use super::http::{self, Url};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Local model servers, where they listen by default.
pub(crate) const KNOWN: &[(&str, &str)] = &[
    ("llama-server", "http://127.0.0.1:8080/v1"),
    ("LM Studio", "http://127.0.0.1:1234/v1"),
    ("Ollama", "http://127.0.0.1:11434/v1"),
    ("MLX or vLLM", "http://127.0.0.1:8000/v1"),
];

/// How a model is asked to act.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
    /// The server's own tool calls.
    Native,
    /// A JSON action in the message ([`ACTION`]).
    Json,
}

pub(crate) struct Model {
    pub url: Url,
    pub name: String,
    pub key: Option<String>,
    /// Tokens the model reads and writes at once.
    pub context: usize,
    pub mode: Mode,
}

/// A call the model asked for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// The model's message: what it says, and the calls it asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
}

/// How a model in [`Mode::Json`] answers.
pub(crate) const ACTION: &str = "\
Answer with one JSON object and nothing else: {\"say\": \"what you tell the person\"} \
to talk, or {\"say\": \"why\", \"call\": {\"tool\": \"<a tool's name>\", \"args\": {...}}} \
to call one of the tools listed below. One call at a time; you will see its result.";

impl Model {
    /// The model at `url` (or the first local server that answers), named
    /// `name` (or the server's first model).
    pub(crate) fn find(url: Option<&str>, name: Option<&str>) -> Result<Self> {
        let probe = Duration::from_millis(400);
        let (url, models) = match url {
            Some(u) => {
                let u = Url::parse(u)?;
                u.ensure_local()?;
                let models = http::get(&u, "/models", Duration::from_secs(5));
                (u, models)
            }
            None => KNOWN
                .iter()
                .find_map(|(_, u)| {
                    let u = Url::parse(u).ok()?;
                    let m = http::get(&u, "/models", probe)?;
                    Some((u, Some(m)))
                })
                .with_context(|| {
                    let tried: Vec<String> = KNOWN
                        .iter()
                        .map(|(what, u)| format!("{u} ({what})"))
                        .collect();
                    format!(
                        "no model server answered at {}; start one, or give its address with --url",
                        tried.join(", ")
                    )
                })?,
        };
        let name = match name {
            Some(n) => n.to_string(),
            None => models
                .as_ref()
                .and_then(|m| m["data"][0]["id"].as_str())
                .map(str::to_string)
                .with_context(|| format!("{} lists no model: name one with --model", url.show()))?,
        };
        Ok(Self {
            url,
            context: DEFAULT_CONTEXT,
            name,
            key: std::env::var("LEGUME_CHAT_KEY").ok(),
            mode: Mode::Native,
        })
    }

    /// The tokens kept for the model's answer.
    pub(crate) fn answer_tokens(&self) -> usize {
        (self.context / 6).clamp(400, 4096)
    }

    /// The model's next message after `messages`, with `tools` (OpenAI
    /// function tools) to call.
    pub(crate) fn next(
        &self,
        messages: &[Value],
        tools: &[Value],
        stop: &AtomicBool,
    ) -> Result<Reply> {
        let mut body = json!({
            "model": self.name,
            "messages": messages,
            "temperature": 0.2,
            "max_tokens": self.answer_tokens(),
            "stream": false,
        });
        match self.mode {
            Mode::Native if !tools.is_empty() => body["tools"] = json!(tools),
            Mode::Native => {}
            Mode::Json => body["response_format"] = json!({ "type": "json_object" }),
        }
        let r = http::post(
            &self.url,
            "/chat/completions",
            &body,
            self.key.as_deref(),
            stop,
        )?;
        let msg = &r["choices"][0]["message"];
        let text = msg["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(match self.mode {
            Mode::Native => Reply {
                text,
                calls: native_calls(msg),
            },
            Mode::Json => action(&text),
        })
    }
}

/// The tokens a model is taken to read unless told (`--context`): few
/// enough for most local models.
const DEFAULT_CONTEXT: usize = 8192;

fn native_calls(msg: &Value) -> Vec<ToolCall> {
    msg["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| {
            let f = &c["function"];
            let args = match &f["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                Value::Object(_) => f["arguments"].clone(),
                _ => json!({}),
            };
            ToolCall {
                id: c["id"]
                    .as_str()
                    .map_or_else(|| format!("call-{i}"), str::to_string),
                name: f["name"].as_str().unwrap_or_default().into(),
                args,
            }
        })
        .collect()
}

/// A [`Mode::Json`] answer read: the first JSON object in it. Anything else
/// is taken as words to the person.
pub(crate) fn action(text: &str) -> Reply {
    let object = text
        .find('{')
        .zip(text.rfind('}'))
        .filter(|(a, b)| a < b)
        .and_then(|(a, b)| serde_json::from_str::<Value>(&text[a..=b]).ok());
    let Some(v) = object else {
        return Reply {
            text: text.into(),
            calls: Vec::new(),
        };
    };
    let say = v["say"].as_str().unwrap_or_default().to_string();
    let calls = match v["call"]["tool"].as_str() {
        Some(tool) => vec![ToolCall {
            id: "call-0".into(),
            name: tool.into(),
            args: match &v["call"]["args"] {
                Value::Object(_) => v["call"]["args"].clone(),
                _ => json!({}),
            },
        }],
        None => Vec::new(),
    };
    Reply { text: say, calls }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_action_is_read_from_whatever_surrounds_it() {
        let r =
            action("Sure. {\"say\": \"looking\", \"call\": {\"tool\": \"status\", \"args\": {}}} ");
        assert_eq!(r.text, "looking");
        assert_eq!(r.calls[0].name, "status");
        let r = action("Done (see the set}. Next: {");
        assert!(r.calls.is_empty() && r.text.starts_with("Done"));
        let r = action("Just words.");
        assert_eq!((r.text.as_str(), r.calls.len()), ("Just words.", 0));
        let r = action("{\"say\": \"hi\"}");
        assert!(r.calls.is_empty());
    }

    #[test]
    fn native_calls_take_their_arguments_as_text_or_json() {
        let msg = json!({ "tool_calls": [
            { "id": "a", "function": { "name": "x", "arguments": "{\"k\":1}" } },
            { "function": { "name": "y", "arguments": { "k": 2 } } },
        ]});
        let calls = native_calls(&msg);
        assert_eq!(calls[0].args, json!({ "k": 1 }));
        assert_eq!(
            (calls[1].id.as_str(), &calls[1].args),
            ("call-1", &json!({ "k": 2 }))
        );
    }
}
