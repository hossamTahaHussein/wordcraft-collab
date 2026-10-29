//! The page's JavaScript API (`wordcraft:call`, `wordcraft:result` and `wordcraft:event`, see
//! `host.rs`): calls run here against the app, and a [`Watcher`] turns what changed since the
//! last frame into events. No browser APIs here, so it is tested natively.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wordcraft_doc::StoryRef;
use wordcraft_engine::Selection;
use wordcraft_ui_egui::WordApp;

/// The formats `getDocument` writes.
const FORMATS: [&str; 7] = ["docx", "odt", "rtf", "pdf", "html", "md", "txt"];

/// What a call answers: a JSON result, and for `getDocument` the file (name and bytes).
#[derive(Debug)]
pub struct Reply {
    pub result: Value,
    pub file: Option<(String, Vec<u8>)>,
}

/// Run API method `method` (`run`, `getText`, `getDocument`, `inspect`, `insertText`, `select`,
/// `setReadOnly`). Commands go through [`WordApp::execute`], the script path: they never ask
/// first, and while co-editing they sync like typed edits.
pub fn call(app: &mut WordApp, method: &str, params: &Value) -> Result<Reply, String> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).ok_or_else(|| format!("`{key}` required"));
    let result = match method {
        "run" => app.execute(text("command")?, params.get("params").cloned().unwrap_or_else(|| json!({})))?,
        "getText" => app.execute("document.text", json!({}))?,
        "inspect" => app.execute("document.inspect", json!({}))?,
        "insertText" => app.execute("text.insert", json!({"text": text("text")?}))?,
        "select" => app.execute("select.text", json!({"text": text("text")?}))?,
        // The page's read only is a lock: the person can't leave it from the Editing menu.
        "setReadOnly" => {
            let on = params.get("on").and_then(Value::as_bool).unwrap_or(true);
            app.execute("view.viewing", json!({"value": on, "lock": on}))?
        }
        "getDocument" => {
            let format = params.get("format").and_then(Value::as_str).unwrap_or("docx").to_ascii_lowercase();
            if !FORMATS.contains(&format.as_str()) {
                return Err(format!("unsupported format `{format}` (one of {})", FORMATS.join(", ")));
            }
            let name = format!("{}.{format}", app.title_stem());
            let bytes = wordcraft_engine::io::save_bytes(&name, &app.session.doc)?;
            return Ok(Reply { result: json!({"name": name}), file: Some((name, bytes)) });
        }
        other => return Err(format!("unknown method `{other}`")),
    };
    Ok(Reply { result, file: None })
}

/// What the page has been told, to tell it what changed.
#[derive(Default)]
pub struct Watcher {
    seen: bool,
    rev: u64,
    sel: Selection,
    /// Each comment and whether it is resolved.
    comments: BTreeMap<u32, bool>,
    people: Vec<(String, u8)>,
}

impl Watcher {
    /// The events since the last call, at most one of each kind but `comment`: `change` when the
    /// document's revision moved (`rev`: the shared revision when co-editing, else the session's;
    /// its author the last of `edited_by`, the people whose edits arrived, else this person),
    /// `selectionchange`, `presence` (the others here, `people`: name and palette index) and a
    /// `comment` per comment added, resolved, reopened or deleted. The first call only records.
    pub fn events(&mut self, app: &WordApp, rev: u64, edited_by: &[String], people: &[(String, u8)]) -> Vec<(&'static str, Value)> {
        let s = &app.session;
        let comments: BTreeMap<u32, bool> = s.doc.comments.iter().map(|(id, c)| (*id, c.resolved)).collect();
        let mut out = Vec::new();
        if self.seen {
            if rev != self.rev {
                let author = edited_by.last().cloned().unwrap_or_else(|| s.author.clone());
                out.push(("change", json!({"rev": rev, "author": author})));
            }
            if s.sel != self.sel {
                out.push(("selectionchange", json!({"text": s.selected_text(), "empty": s.sel.is_collapsed()})));
            }
            for (id, resolved) in &comments {
                let action = match self.comments.get(id) {
                    None => "added",
                    Some(was) if was != resolved => {
                        if *resolved {
                            "resolved"
                        } else {
                            "reopened"
                        }
                    }
                    Some(_) => continue,
                };
                out.push(("comment", comment(app, *id, action)));
            }
            for id in self.comments.keys().filter(|id| !comments.contains_key(id)) {
                out.push(("comment", json!({"action": "deleted", "id": id, "author": "", "text": ""})));
            }
            if people != self.people.as_slice() {
                let list: Vec<Value> =
                    people.iter().map(|(name, color)| json!({"name": name, "color": wordcraft_ui_egui::peer_css_color(*color)})).collect();
                out.push(("presence", json!({"people": list})));
            }
        }
        self.seen = true;
        self.rev = rev;
        self.sel = s.sel.clone();
        self.comments = comments;
        self.people = people.to_vec();
        out
    }
}

/// A comment as the `comment` event describes it.
fn comment(app: &WordApp, id: u32, action: &str) -> Value {
    let doc = &app.session.doc;
    let (author, text) = doc.comments.get(&id).map_or((String::new(), String::new()), |c| (c.author.clone(), doc.plain_text(StoryRef::Part(c.part))));
    json!({"action": action, "id": id, "author": author, "text": text})
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wordcraft_ui_egui::{Services, WordApp};

    use super::{Watcher, call};

    fn app() -> WordApp {
        WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Services::default())
    }

    fn ok(app: &mut WordApp, method: &str, params: Value) -> Value {
        call(app, method, &params).unwrap_or_else(|e| panic!("{method}: {e}")).result
    }

    #[test]
    fn calls_run_commands_and_read_the_document() {
        let mut a = app();
        ok(&mut a, "insertText", json!({"text": "Hi there"}));
        assert_eq!(ok(&mut a, "getText", json!({}))["text"], "Hi there");
        ok(&mut a, "select", json!({"text": "there"}));
        assert_eq!(ok(&mut a, "run", json!({"command": "document.selection"}))["text"], "there");
        assert!(ok(&mut a, "inspect", json!({})).is_object());
    }

    #[test]
    fn get_document_returns_bytes_in_each_format() {
        let mut a = app();
        ok(&mut a, "insertText", json!({"text": "Hi"}));
        let file = |a: &mut WordApp, f: &str| call(a, "getDocument", &json!({"format": f})).map(|r| r.file.expect("bytes"));
        let (name, bytes) = file(&mut a, "docx").unwrap();
        assert!(name.ends_with(".docx") && bytes.starts_with(b"PK"), "{name}");
        assert!(file(&mut a, "pdf").unwrap().1.starts_with(b"%PDF"));
        assert!(String::from_utf8_lossy(&file(&mut a, "txt").unwrap().1).contains("Hi"));
        for f in ["odt", "rtf", "html", "md"] {
            assert!(!file(&mut a, f).unwrap().1.is_empty(), "{f}");
        }
        assert!(file(&mut a, "exe").unwrap_err().contains("unsupported format"));
    }

    #[test]
    fn a_failing_call_reports_its_error() {
        let mut a = app();
        assert!(call(&mut a, "run", &json!({"command": "no.such"})).is_err());
        assert!(call(&mut a, "nothing", &json!({})).unwrap_err().contains("unknown method"));
        assert!(call(&mut a, "getText", &json!({})).is_ok());
    }

    #[test]
    fn set_read_only_refuses_edits() {
        let mut a = app();
        ok(&mut a, "setReadOnly", json!({"on": true}));
        let e = call(&mut a, "insertText", &json!({"text": "x"})).unwrap_err();
        assert!(e.contains("open for viewing"), "{e}");
        ok(&mut a, "setReadOnly", json!({"on": false}));
        ok(&mut a, "insertText", json!({"text": "x"}));
    }

    fn names(events: &[(&'static str, Value)]) -> Vec<&'static str> {
        events.iter().map(|e| e.0).collect()
    }

    #[test]
    fn one_change_event_per_frame_and_none_when_idle() {
        let mut a = app();
        let mut w = Watcher::default();
        assert!(w.events(&a, a.session.rev(), &[], &[]).is_empty(), "the first look only records");
        for t in ["a", "b", "c"] {
            ok(&mut a, "insertText", json!({"text": t}));
        }
        let ev = w.events(&a, a.session.rev(), &[], &[]);
        let changes: Vec<&Value> = ev.iter().filter(|e| e.0 == "change").map(|e| &e.1).collect();
        assert_eq!(changes.len(), 1, "{ev:?}");
        assert_eq!(changes[0]["rev"], a.session.rev());
        assert_eq!(changes[0]["author"], a.session.author.as_str());
        assert!(!names(&w.events(&a, a.session.rev(), &[], &[])).contains(&"change"));
        // Someone else's edit names them.
        ok(&mut a, "insertText", json!({"text": "d"}));
        let ev = w.events(&a, a.session.rev(), &["Ann".to_string()], &[]);
        assert_eq!(ev.iter().find(|e| e.0 == "change").map(|e| e.1["author"].clone()), Some(json!("Ann")));
    }

    #[test]
    fn selection_comment_and_presence_events() {
        let mut a = app();
        ok(&mut a, "insertText", json!({"text": "Hello world"}));
        let mut w = Watcher::default();
        w.events(&a, a.session.rev(), &[], &[]);
        ok(&mut a, "select", json!({"text": "world"}));
        let ev = w.events(&a, a.session.rev(), &[], &[]);
        let sel = ev.iter().find(|e| e.0 == "selectionchange").expect("selectionchange");
        assert_eq!(sel.1, json!({"text": "world", "empty": false}));
        ok(&mut a, "run", json!({"command": "review.newComment", "params": {"text": "Note"}}));
        let ev = w.events(&a, a.session.rev(), &[], &[]);
        let added = ev.iter().find(|e| e.0 == "comment").expect("comment").1.clone();
        assert_eq!(added["action"], "added");
        assert_eq!(added["text"], "Note");
        let id = added["id"].as_u64().expect("id");
        ok(&mut a, "run", json!({"command": "review.resolveComment", "params": {"id": id, "value": true}}));
        let ev = w.events(&a, a.session.rev(), &[], &[]);
        assert_eq!(ev.iter().find(|e| e.0 == "comment").map(|e| e.1["action"].clone()), Some(json!("resolved")));
        let ev = w.events(&a, a.session.rev(), &[], &[("Ann".to_string(), 2)]);
        let presence = ev.iter().find(|e| e.0 == "presence").expect("presence");
        assert_eq!(presence.1, json!({"people": [{"name": "Ann", "color": wordcraft_ui_egui::peer_css_color(2)}]}));
        assert!(!names(&w.events(&a, a.session.rev(), &[], &[("Ann".to_string(), 2)])).contains(&"presence"));
    }
}

#[cfg(test)]
mod lock_tests {
    use serde_json::json;
    use wordcraft_ui_egui::{Services, WordApp};

    use super::call;

    /// setReadOnly(true) holds: the Editing menu (view.viewing) can't turn it off.
    #[test]
    fn read_only_from_the_page_cannot_be_turned_off_by_the_person() {
        let mut a = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Services::default());
        call(&mut a, "setReadOnly", &json!({"on": true})).unwrap();
        assert!(a.run("view.viewing", json!({"value": false})).is_err());
        assert!(call(&mut a, "insertText", &json!({"text": "x"})).is_err());
        call(&mut a, "setReadOnly", &json!({"on": false})).unwrap();
        call(&mut a, "insertText", &json!({"text": "x"})).unwrap();
    }
}
