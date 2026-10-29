//! A stand-in model server for the chat's tests: an OpenAI-compatible
//! `/v1` on a local port that answers each chat request with the next of
//! the replies it was given, and keeps the requests.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A model server that answers each chat request with the next of
/// `replies` (an assistant message, or an error status with its text),
/// keeping the requests.
pub struct Stand {
    pub url: String,
    pub asked: Arc<Mutex<Vec<Value>>>,
}

pub enum Answer {
    Message(Value),
    Refuse(u16, &'static str),
}

pub fn stand(replies: Vec<Answer>) -> Stand {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let asked: Arc<Mutex<Vec<Value>>> = Arc::default();
    let kept = asked.clone();
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let (status, text) = if first.starts_with("GET") {
                (200, json!({ "data": [{ "id": "stand-in" }] }).to_string())
            } else {
                kept.lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                match replies.next() {
                    Some(Answer::Message(m)) => {
                        (200, json!({ "choices": [{ "message": m }] }).to_string())
                    }
                    Some(Answer::Refuse(code, why)) => (code, json!({ "error": why }).to_string()),
                    None => (500, "no more replies".to_string()),
                }
            };
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
                text.len()
            );
        }
    });
    Stand { url, asked }
}

pub fn calls(name: &str, args: Value) -> Answer {
    Answer::Message(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{ "id": format!("c-{name}"), "type": "function",
                         "function": { "name": name, "arguments": args.to_string() } }],
    }))
}

pub fn says(text: &str) -> Answer {
    Answer::Message(json!({ "role": "assistant", "content": text }))
}
