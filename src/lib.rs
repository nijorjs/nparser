//! nparser — a lightweight HTML-like DOM parser exposed to Node.js via napi-rs.
//!
//! Custom parsing rules (vs standard HTML):
//!   • Self-closing tags honoured: <widget foo="bar"/> creates a childless node
//!   • Boolean attributes kept verbatim: <input disabled> ≠ <input disabled="">
//!   • '<' '>' '&' are never entity-encoded / decoded
//!   • Comments <!-- ... --> become raw text nodes (not discarded)

#![allow(clippy::all)]

mod arena;
mod parser;
mod selector;

use napi_derive::napi;
use std::sync::{Arc, Mutex};

use arena::{Arena, NodeData, NodeId};
use parser::{parse_err, Parser};

type SharedArena = Arc<Mutex<Arena>>;

// ── JsElement ────────────────────────────────────────────────────

/// A handle to a single DOM node (element, text, or document root).
#[napi]
pub struct JsElement {
    arena: SharedArena,
    id: usize,
}

#[napi]
impl JsElement {
    // ── tagName ──────────────────────────────────────────────────

    /// Lowercase tag name, or null for text/document nodes.
    #[napi(getter)]
    pub fn tag_name(&self) -> Option<String> {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Element { tag, .. } => Some(tag.clone()),
            _ => None,
        }
    }

    // ── nodeType ─────────────────────────────────────────────────

    /// 1 = Element, 3 = Text, 9 = Document
    #[napi(getter)]
    pub fn node_type(&self) -> u32 {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Document => 9,
            NodeData::Element { .. } => 1,
            NodeData::Text(_) => 3,
        }
    }

    // ── textContent ──────────────────────────────────────────────

    #[napi(getter)]
    pub fn text_content(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.text_content(NodeId(self.id))
    }

    // ── innerHTML ────────────────────────────────────────────────
    // Exposed as getInnerHtml / setInnerHtml plain napi methods.
    // The JS wrapper (nparser.js) installs Object.defineProperty so
    // that `el.innerHTML = "..."` and `el.innerHTML` work across
    // both Node.js and Bun.

    #[napi(js_name = "getInnerHtml")]
    pub fn get_inner_html(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.inner_html(NodeId(self.id))
    }

    #[napi(js_name = "setInnerHtml")]
    pub fn set_inner_html(&mut self, html: String) -> napi::Result<()> {
        let mut arena = self.arena.lock().unwrap();
        let id = NodeId(self.id);
        arena.clear_children(id);
        Parser::new(html)
            .parse_children(&mut arena, id)
            .map_err(parse_err)?;
        Ok(())
    }

    // ── outerHTML ────────────────────────────────────────────────

    #[napi(getter)]
    pub fn outer_html(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.outer_html(NodeId(self.id))
    }

    // ── Attributes ───────────────────────────────────────────────

    #[napi]
    pub fn get_attribute(&self, name: String) -> Option<String> {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Element { attrs, .. } => attrs.get(&name).and_then(|v| v.clone()),
            _ => None,
        }
    }

    /// Returns true even for boolean attributes that have no value.
    #[napi]
    pub fn has_attribute(&self, name: String) -> bool {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Element { attrs, .. } => attrs.contains_key(&name),
            _ => false,
        }
    }

    /// Pass `null` as value to set a boolean attribute with no value.
    #[napi]
    pub fn set_attribute(&mut self, name: String, value: Option<String>) {
        let mut arena = self.arena.lock().unwrap();
        if let NodeData::Element { attrs, .. } = &mut arena.get_mut(NodeId(self.id)).data {
            attrs.insert(name, value);
        }
    }

    #[napi]
    pub fn remove_attribute(&mut self, name: String) {
        let mut arena = self.arena.lock().unwrap();
        if let NodeData::Element { attrs, .. } = &mut arena.get_mut(NodeId(self.id)).data {
            attrs.remove(&name);
        }
    }

    // ── Children / Parent ────────────────────────────────────────

    #[napi]
    pub fn children(&self) -> Vec<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena
            .get(NodeId(self.id))
            .children
            .iter()
            .map(|&c| JsElement { arena: Arc::clone(&self.arena), id: c.0 })
            .collect()
    }

    #[napi(getter)]
    pub fn parent(&self) -> Option<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena.get(NodeId(self.id)).parent.map(|p| JsElement {
            arena: Arc::clone(&self.arena),
            id: p.0,
        })
    }

    // ── querySelector / querySelectorAll ─────────────────────────

    #[napi]
    pub fn query_selector(&self, selector: String) -> Option<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena.query_selector(NodeId(self.id), &selector).map(|found| JsElement {
            arena: Arc::clone(&self.arena),
            id: found.0,
        })
    }

    #[napi]
    pub fn query_selector_all(&self, selector: String) -> Vec<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena
            .query_selector_all(NodeId(self.id), &selector)
            .into_iter()
            .map(|found| JsElement { arena: Arc::clone(&self.arena), id: found.0 })
            .collect()
    }
}

// ── JsDocument ───────────────────────────────────────────────────

/// The parsed document — equivalent of `document` in the browser.
#[napi]
pub struct JsDocument {
    arena: SharedArena,
    root: usize,
}

#[napi]
impl JsDocument {
    /// Parse an HTML-like string and return a JsDocument.
    #[napi(factory)]
    pub fn parse(html: String) -> napi::Result<JsDocument> {
        let mut arena = Arena::new();
        let root = arena.root;
        Parser::new(html)
            .parse_children(&mut arena, root)
            .map_err(parse_err)?;
        Ok(JsDocument {
            arena: Arc::new(Mutex::new(arena)),
            root: root.0,
        })
    }

    /// The document root (nodeType 9).
    #[napi(getter)]
    pub fn document_element(&self) -> JsElement {
        JsElement { arena: Arc::clone(&self.arena), id: self.root }
    }

    #[napi]
    pub fn query_selector(&self, selector: String) -> Option<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena.query_selector(NodeId(self.root), &selector).map(|found| JsElement {
            arena: Arc::clone(&self.arena),
            id: found.0,
        })
    }

    #[napi]
    pub fn query_selector_all(&self, selector: String) -> Vec<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena
            .query_selector_all(NodeId(self.root), &selector)
            .into_iter()
            .map(|found| JsElement { arena: Arc::clone(&self.arena), id: found.0 })
            .collect()
    }

    /// Serialise the entire document back to a string.
    #[napi]
    pub fn serialize(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.inner_html(NodeId(self.root))
    }
}