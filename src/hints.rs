//! What a server tells an agent beyond its tools' results: how each tool
//! behaves ([`Annotations`]), the calls that could come next ([`Next`]), and
//! recipes for whole pieces of work ([`Prompt`], MCP prompts, which a
//! client shows as commands such as `/mcp__legume__explore`).

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// How a tool behaves, as MCP's tool annotations say it: hints a client
/// uses to decide what to ask the user before a call. Unset ones are not
/// sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Annotations {
    pub title: Option<String>,
    /// It changes nothing.
    pub read_only: Option<bool>,
    /// It may remove or replace something (it is never set for a tool that
    /// only writes new files, as the legume tools do).
    pub destructive: Option<bool>,
    /// Calling it again with the same arguments does nothing more.
    pub idempotent: Option<bool>,
    /// It reaches outside the project (a web service, a download).
    pub open_world: Option<bool>,
}

impl Annotations {
    pub(crate) fn to_json(&self) -> Option<Value> {
        let mut m = Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        };
        put("title", self.title.clone().map(Value::from));
        put("readOnlyHint", self.read_only.map(Value::from));
        put("destructiveHint", self.destructive.map(Value::from));
        put("idempotentHint", self.idempotent.map(Value::from));
        put("openWorldHint", self.open_world.map(Value::from));
        (!m.is_empty()).then_some(Value::Object(m))
    }
}

/// A call that could come next, for a tool's result to offer: the tool
/// (on another server, as `lupin annotate_run`), its arguments, and why.
/// The agent proposes it; the person chooses.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Next {
    pub tool: String,
    pub args: Value,
    pub why: String,
}

impl Next {
    pub fn new(tool: &str, args: Value, why: &str) -> Self {
        Self {
            tool: tool.into(),
            args,
            why: why.into(),
        }
    }
}

/// The text the agent reads for `next`.
pub(crate) fn next_text(next: &[Next]) -> String {
    let mut out = String::from(
        "Next, if the person agrees (say which you propose and why; when the choice is \
         theirs, offer these as options):\n",
    );
    for (i, n) in next.iter().enumerate() {
        let args = match &n.args {
            Value::Object(m) if m.is_empty() => String::new(),
            Value::Null => String::new(),
            a => format!(" {a}"),
        };
        out.push_str(&format!("{}. {}{args}: {}\n", i + 1, n.tool, n.why));
    }
    out
}

/// An argument of a [`Prompt`].
#[derive(Clone, Debug)]
pub struct PromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// A recipe for a piece of work, as MCP prompts give it: its text, with
/// `{name}` where each argument goes. An argument not given reads "not
/// given".
#[derive(Clone, Debug)]
pub struct Prompt {
    pub name: String,
    pub title: String,
    pub description: String,
    pub arguments: Vec<PromptArg>,
    pub text: String,
}

impl Prompt {
    pub fn new(name: &str, title: &str, description: &str, text: &str) -> Self {
        Self {
            name: name.into(),
            title: title.into(),
            description: description.into(),
            arguments: Vec::new(),
            text: text.into(),
        }
    }

    /// An argument, `{name}` in the text.
    pub fn arg(mut self, name: &str, description: &str, required: bool) -> Self {
        self.arguments.push(PromptArg {
            name: name.into(),
            description: description.into(),
            required,
        });
        self
    }

    /// As `prompts/list` shows it.
    pub(crate) fn listed(&self) -> Value {
        let args: Vec<Value> = self
            .arguments
            .iter()
            .map(
                |a| json!({ "name": a.name, "description": a.description, "required": a.required }),
            )
            .collect();
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "arguments": args,
        })
    }

    /// As `prompts/get` gives it, with `args` filled in.
    pub(crate) fn get(&self, args: &Value) -> Result<Value> {
        let mut text = self.text.clone();
        for a in &self.arguments {
            let given = args[&a.name]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            anyhow::ensure!(
                given.is_some() || !a.required,
                "`{}` is required: {}",
                a.name,
                a.description
            );
            text = text.replace(&format!("{{{}}}", a.name), given.unwrap_or("not given"));
        }
        Ok(json!({
            "description": self.description,
            "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_fills_its_arguments_and_asks_for_the_required_ones() {
        let p = Prompt::new("x", "X", "does x", "Q: {question}. Data: {data}.")
            .arg("question", "the question", true)
            .arg("data", "the data", false);
        let got = p.get(&json!({ "question": "why" })).unwrap();
        assert_eq!(
            got["messages"][0]["content"]["text"],
            "Q: why. Data: not given."
        );
        let err = p.get(&json!({})).unwrap_err().to_string();
        assert!(err.contains("`question` is required"), "{err}");
        assert_eq!(p.listed()["arguments"][1]["required"], false);
    }

    #[test]
    fn only_the_hints_set_are_sent() {
        assert_eq!(Annotations::default().to_json(), None);
        let a = Annotations {
            read_only: Some(true),
            ..Default::default()
        };
        assert_eq!(a.to_json(), Some(json!({ "readOnlyHint": true })));
        let text = next_text(&[
            Next::new("job_wait", json!({ "id": "job-1" }), "it is still running"),
            Next::new("lupin annotate_run", json!({}), "name the clusters"),
        ]);
        assert!(text.contains("1. job_wait {\"id\":\"job-1\"}: it is still running\n"));
        assert!(text.contains("2. lupin annotate_run: name the clusters\n"));
    }
}
