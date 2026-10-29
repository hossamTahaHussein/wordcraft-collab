//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! WordCraft in an `<iframe>` and keep its files (a Nextcloud app, a personal file server). The
//! same messages as GridCraft's and PdfCraft's bridges; each is a plain object with a `type`:
//!
//! - `wordcraft:ready` (to the parent): `{ version }`, sent once the app is running and
//!   listening; send a file now.
//! - `wordcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a document picked with File › Open or dropped on the window.
//! - `wordcraft:save` (to the parent): `{ name, saveAs, bytes }` (a transferred `ArrayBuffer`) for
//!   every file WordCraft would otherwise download. The parent stores it and reports failures
//!   itself. `saveAs` says whether the file becomes the document's file:
//!   - Save: `false`. It writes the opened file under its own name and format, as the desktop
//!     app does. A name the parent never sent (an untitled document, an imported .doc saved as
//!     .docx) is a new file: `true`, as for Save As.
//!   - Save As, and File › Export to another document format (which runs Save As): `true` for a
//!     format that keeps everything (.docx and the other Word packages, .odt, .rtf). The document
//!     is now that file; the next Save sends its name with `false`.
//!   - Save As or Export to a format that doesn't (PDF, Markdown, plain text), Export as PDF and
//!     Export Page as PNG: `false`. They are copies; the document's file stays the same.
//! - `wordcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `wordcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//! - `wordcraft:collab` (both ways, with `?collab=1`): `{ msg }`, one co-editing relay message
//!   as a JSON string (`wordcraft_collab::proto`); the parent carries them to and from the relay.
//! - `wordcraft:collab.status` (to the parent): `{ pending, rev, people }` when any changes.
//! - `wordcraft:call` (from the parent): `{ id, method, params }`, a JavaScript API call
//!   (`crate::api::call`); answered with `wordcraft:result` `{ id, ok: true, result }` or
//!   `{ id, ok: false, error }`. `getDocument` adds `name` and `bytes` (a transferred
//!   `ArrayBuffer`).
//! - `wordcraft:event` (to the parent): `{ event, detail }`, what changed this frame
//!   (`crate::api::Watcher`): `change`, `selectionchange`, `presence` and `comment`.
//!
//! Only the parent window on the page's own origin is heard or answered. In host mode the parent
//! guards leaving the page, and `?author=` names the user for comments and tracked changes.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wordcraft_ui_egui::{Inbox, Services, WordApp};

/// The longest user name taken from `?author=`.
const MAX_AUTHOR_CHARS: usize = 200;

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// Co-editing messages from the parent, waiting for the next frame.
    collab: Rc<RefCell<VecDeque<String>>>,
    /// API calls from the parent (id, method, params), waiting for the next frame.
    calls: Rc<RefCell<VecDeque<(f64, String, serde_json::Value)>>>,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to), or when the parent is
    /// on another origin: messages only go to this origin, so the browser would drop every save
    /// without an error while the document showed as saved.
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        // An opaque origin (a `file://` page in some browsers) can't be a `postMessage` target.
        if origin == "null" {
            return None;
        }
        // Reading another origin's location throws, which is `None` too.
        if parent.location().origin().ok()? != origin {
            return None;
        }
        Some(Self { window, parent, origin, collab: Rc::default(), calls: Rc::default() })
    }

    /// Hand saved and exported files to the parent instead of downloading them, report unsaved
    /// changes to it, and hear the files it sends.
    pub fn connect(&self, services: &mut Services, inbox: Inbox, ctx: egui::Context) {
        self.listen(inbox, ctx);
        let bridge = self.clone();
        services.host_save = Some(Box::new(move |name: &str, bytes: &[u8], save_as: bool| bridge.post_save(name, bytes, save_as)));
        // Anything else that would download goes to the parent too.
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| bridge.post_save(name, bytes, false)));
        // The browser's save picker is never used, so no save finishes later.
        services.async_saves = None;
        let bridge = self.clone();
        let reported: Rc<Cell<Option<bool>>> = Rc::default();
        services.on_dirty = Some(Box::new(move |dirty: bool| {
            if reported.get() != Some(dirty) && bridge.send(&[("type", "wordcraft:dirty".into()), ("dirty", dirty.into())]) {
                reported.set(Some(dirty));
            }
        }));
    }

    /// `?author=`: the host's name for the user signs comments and tracked changes.
    pub fn set_author(app: &mut WordApp, author: &str) {
        let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
        if !author.is_empty() {
            app.session.author = author;
        }
    }

    pub fn post_ready(&self) {
        self.send(&[("type", "wordcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
    }

    /// The app couldn't start (no WebGPU or WebGL2, for example): let the parent say so.
    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "wordcraft:failed".into()), ("error", error.into())]);
    }

    pub fn post_save(&self, path: &str, bytes: &[u8], save_as: bool) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let message = message(&[
            ("type", "wordcraft:save".into()),
            ("name", name.as_str().into()),
            ("saveAs", save_as.into()),
            ("bytes", buffer.clone().into()),
        ])
        .ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }

    /// The API calls the parent sent since the last frame.
    pub fn take_calls(&self) -> Vec<(f64, String, serde_json::Value)> {
        self.calls.borrow_mut().drain(..).collect()
    }

    /// Answer API call `id`; a file goes as a transferred `ArrayBuffer`.
    pub fn post_result(&self, id: f64, reply: Result<crate::api::Reply, String>) {
        let id_value = JsValue::from_f64(id);
        let id = id_value.clone();
        let sent = match reply {
            Ok(crate::api::Reply { result, file }) => {
                let result = js_sys::JSON::parse(&result.to_string()).unwrap_or(JsValue::NULL);
                let mut fields = vec![("type", "wordcraft:result".into()), ("id", id), ("ok", true.into()), ("result", result)];
                match file {
                    Some((name, bytes)) => {
                        let buffer = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
                        fields.push(("name", name.as_str().into()));
                        fields.push(("bytes", buffer.clone().into()));
                        message(&fields)
                            .is_some_and(|m| self.parent.post_message_with_transfer(&m, &self.origin, &js_sys::Array::of1(&buffer)).is_ok())
                    }
                    None => self.send(&fields),
                }
            }
            Err(error) => self.send(&[("type", "wordcraft:result".into()), ("id", id), ("ok", false.into()), ("error", error.as_str().into())]),
        };
        if !sent {
            // The page still hears that the call failed, instead of waiting for its timeout.
            log::warn!("api: couldn't answer a call");
            let _ = self.send(&[
                ("type", "wordcraft:result".into()),
                ("id", id_value),
                ("ok", false.into()),
                ("error", "couldn't send the result".into()),
            ]);
        }
    }

    /// Tell the parent what changed (`wordcraft:event`).
    pub fn post_event(&self, event: &str, detail: &serde_json::Value) {
        let detail = js_sys::JSON::parse(&detail.to_string()).unwrap_or(JsValue::NULL);
        self.send(&[("type", "wordcraft:event".into()), ("event", event.into()), ("detail", detail)]);
    }

    /// The co-editing messages the parent sent since the last call.
    pub fn take_collab(&self) -> Vec<String> {
        self.collab.borrow_mut().drain(..).collect()
    }

    /// Send a co-editing message to the parent for the relay.
    pub fn post_collab(&self, msg: &str) {
        self.send(&[("type", "wordcraft:collab".into()), ("msg", msg.into())]);
    }

    /// `people`: everyone in the document, this person included.
    pub fn post_collab_status(&self, pending: usize, rev: Option<u64>, people: usize) {
        let rev = rev.map_or(JsValue::NULL, |r| JsValue::from_f64(r as f64));
        self.send(&[
            ("type", "wordcraft:collab.status".into()),
            ("pending", JsValue::from_f64(pending as f64)),
            ("rev", rev),
            ("people", JsValue::from_f64(people as f64)),
        ]);
    }

    /// Hear `wordcraft:open` from the parent and queue the file like a dropped one, and queue
    /// `wordcraft:collab` messages for the next frame.
    fn listen(&self, inbox: Inbox, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let collab = self.collab.clone();
        let calls = self.calls.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            match string(&data, "type").as_deref() {
                Some("wordcraft:open") => {}
                Some("wordcraft:call") => {
                    let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).ok().and_then(|v| v.as_f64());
                    let method = string(&data, "method");
                    // No params is `{}`; anything but an object (or something that can't be read as
                    // JSON) is passed on as null, which the call refuses.
                    let params = match js_sys::Reflect::get(&data, &JsValue::from_str("params")) {
                        Ok(p) if p.is_undefined() || p.is_null() => serde_json::json!({}),
                        Ok(p) if p.is_object() && !js_sys::Array::is_array(&p) => js_sys::JSON::stringify(&p)
                            .ok()
                            .and_then(|p| p.as_string())
                            .and_then(|p| serde_json::from_str(&p).ok())
                            .unwrap_or(serde_json::Value::Null),
                        _ => serde_json::Value::Null,
                    };
                    if let (Some(id), Some(method)) = (id, method) {
                        calls.borrow_mut().push_back((id, method, params));
                        ctx.request_repaint();
                    }
                    return;
                }
                Some("wordcraft:collab") => {
                    if let Some(msg) = string(&data, "msg") {
                        collab.borrow_mut().push_back(msg);
                        ctx.request_repaint();
                    }
                    return;
                }
                _ => return,
            }
            let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Document.docx".to_string());
            let Some(bytes) = bytes(&data) else { return };
            inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
            ctx.request_repaint();
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}
