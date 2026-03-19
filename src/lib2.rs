//! nparser — a lightweight HTML-like DOM parser exposed to Node.js via napi-rs.
//!
//! Custom parsing rules (vs standard HTML):
//!   • Self-closing tags honoured: <widget foo="bar"/> creates a childless node
//!   • Boolean attributes kept verbatim: <input disabled> ≠ <input disabled="">
//!   • '<' '>' '&' are never entity-encoded / decoded
//!   • Comments <!-- ... --> become raw text nodes (not discarded)

#![allow(clippy::all)]

// use napi::bindgen_prelude::ToNapiValue;
use napi_derive::napi;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

// Internal Result alias — avoids collision with napi's Result<T, E: AsRef<str>>.
type PResult<T> = std::result::Result<T, ParseError>;

// ══════════════════════════════════════════════════════════════════
//  Internal parse error
// ══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
enum ParseError {
    UnexpectedEof,
    InvalidTagName(String),
    MismatchedTag { opened: String, closed: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "Unexpected end of input"),
            Self::InvalidTagName(s) => write!(f, "Invalid tag name: {:?}", s),
            Self::MismatchedTag { opened, closed } => {
                write!(f, "Mismatched tag: opened <{}>, closed </{}>", opened, closed)
            }
        }
    }
}

fn parse_err(e: ParseError) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, e.to_string())
}

// ══════════════════════════════════════════════════════════════════
//  Internal arena model
// ══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NodeId(usize);

#[derive(Debug, Clone)]
enum NodeData {
    Document,
    Element {
        tag: String,
        attrs: HashMap<String, Option<String>>,
    },
    Text(String),
}

#[derive(Debug, Clone)]
struct InternalNode {
    data: NodeData,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

#[derive(Debug, Clone)]
struct Arena {
    nodes: Vec<InternalNode>,
    root: NodeId,
}

impl Arena {
    fn new() -> Self {
        let root_node = InternalNode {
            data: NodeData::Document,
            parent: None,
            children: vec![],
        };
        Arena {
            nodes: vec![root_node],
            root: NodeId(0),
        }
    }

    fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(InternalNode {
            data,
            parent: None,
            children: vec![],
        });
        id
    }

    fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[child.0].parent = Some(parent);
        self.nodes[parent.0].children.push(child);
    }

    fn get(&self, id: NodeId) -> &InternalNode {
        &self.nodes[id.0]
    }

    fn get_mut(&mut self, id: NodeId) -> &mut InternalNode {
        &mut self.nodes[id.0]
    }

    fn clear_children(&mut self, parent: NodeId) {
        let children = std::mem::take(&mut self.nodes[parent.0].children);
        for child in children {
            self.nodes[child.0].parent = None;
            self.clear_children(child);
        }
    }

    // ── serialisation ───────────────────────────────────────────

    fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for &child in &self.nodes[id.0].children {
            self.serialise_node(child, &mut out);
        }
        out
    }

    fn outer_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.serialise_node(id, &mut out);
        out
    }

    fn serialise_node(&self, id: NodeId, out: &mut String) {
        let node = &self.nodes[id.0];
        match &node.data {
            NodeData::Document => {
                for &child in &node.children {
                    self.serialise_node(child, out);
                }
            }
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element { tag, attrs } => {
                out.push('<');
                out.push_str(tag);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    if let Some(val) = v {
                        out.push_str("=\"");
                        out.push_str(val);
                        out.push('"');
                    }
                }
                if node.children.is_empty() && is_void(tag) {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    for &child in &node.children {
                        self.serialise_node(child, out);
                    }
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                }
            }
        }
    }

    // ── text content ────────────────────────────────────────────

    fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        let node = &self.nodes[id.0];
        match &node.data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for &child in &node.children {
                    self.collect_text(child, out);
                }
            }
        }
    }

    // ── querySelector ───────────────────────────────────────────

    fn query_selector(&self, start: NodeId, selector: &str) -> Option<NodeId> {
        let sel = Selector::parse(selector);
        let mut result = None;
        self.walk_descendants(start, &mut |id| {
            if result.is_none() && sel.matches(self, id) {
                result = Some(id);
            }
        });
        result
    }

    fn query_selector_all(&self, start: NodeId, selector: &str) -> Vec<NodeId> {
        let sel = Selector::parse(selector);
        let mut results = Vec::new();
        self.walk_descendants(start, &mut |id| {
            if sel.matches(self, id) {
                results.push(id);
            }
        });
        results
    }

    fn walk_descendants<F: FnMut(NodeId)>(&self, id: NodeId, f: &mut F) {
        for &child in &self.nodes[id.0].children {
            f(child);
            self.walk_descendants(child, f);
        }
    }
}

// ══════════════════════════════════════════════════════════════════
//  Tokeniser / Parser
// ══════════════════════════════════════════════════════════════════

struct Parser {
    src: String,
    pos: usize,
}

impl Parser {
    fn new(src: impl Into<String>) -> Self {
        Parser { src: src.into(), pos: 0 }
    }

    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }

    fn consume_while<F: Fn(char) -> bool>(&mut self, pred: F) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if pred(c) {
                out.push(c);
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        out
    }

    fn skip_whitespace(&mut self) {
        self.consume_while(|c| c.is_ascii_whitespace());
    }

    fn parse_children(
        &mut self,
        arena: &mut Arena,
        parent: NodeId,
    ) -> PResult<Option<String>> {
        loop {
            if self.rest().is_empty() {
                return Ok(None);
            }
            if self.starts_with("</") {
                return Ok(Some(self.parse_close_tag()?));
            }
            if self.starts_with("<!--") {
                let text = self.consume_comment();
                let id = arena.alloc(NodeData::Text(text));
                arena.append_child(parent, id);
                continue;
            }
            if self.starts_with("<") {
                self.parse_element(arena, parent)?;
                continue;
            }
            let text = self.consume_text();
            if !text.is_empty() {
                let id = arena.alloc(NodeData::Text(text));
                arena.append_child(parent, id);
            }
        }
    }

    fn consume_text(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '<' {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.src[start..self.pos].to_string()
    }

    fn consume_comment(&mut self) -> String {
        let start = self.pos;
        self.pos += 4; // <!--
        loop {
            if self.starts_with("-->") {
                self.pos += 3;
                break;
            }
            if self.rest().is_empty() {
                break;
            }
            let c = self.peek().unwrap();
            self.pos += c.len_utf8();
        }
        self.src[start..self.pos].to_string()
    }

    fn parse_close_tag(&mut self) -> PResult<String> {
        self.pos += 2; // </
        self.skip_whitespace();
        let name = self.consume_while(is_name_char);
        self.skip_whitespace();
        if self.peek() == Some('>') {
            self.advance();
        }
        Ok(name)
    }

    fn parse_element(&mut self, arena: &mut Arena, parent: NodeId) -> PResult<()> {
        self.advance(); // <
        let tag = self.consume_while(is_name_char);
        if tag.is_empty() {
            return Err(ParseError::InvalidTagName(tag));
        }
        let tag = tag.to_ascii_lowercase();
        let attrs = self.parse_attrs()?;

        // Explicit self-close />
        if self.starts_with("/>") {
            self.pos += 2;
            let id = arena.alloc(NodeData::Element { tag, attrs });
            arena.append_child(parent, id);
            return Ok(());
        }

        if self.peek() == Some('>') {
            self.advance();
        }

        // Void / self-closing by convention
        if is_void(&tag) {
            let id = arena.alloc(NodeData::Element { tag, attrs });
            arena.append_child(parent, id);
            return Ok(());
        }

        let elem_id = arena.alloc(NodeData::Element { tag: tag.clone(), attrs });
        arena.append_child(parent, elem_id);

        let close = self.parse_children(arena, elem_id)?;
        if let Some(close_tag) = close {
            if !close_tag.is_empty() && close_tag != tag {
                return Err(ParseError::MismatchedTag {
                    opened: tag,
                    closed: close_tag,
                });
            }
        }
        Ok(())
    }

    fn parse_attrs(&mut self) -> PResult<HashMap<String, Option<String>>> {
        let mut attrs = HashMap::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(ParseError::UnexpectedEof),
                Some('>') | Some('/') => break,
                _ => {}
            }
            let name = self.consume_while(is_attr_name_char);
            if name.is_empty() {
                break;
            }
            let name = name.to_ascii_lowercase();
            self.skip_whitespace();
            if self.peek() == Some('=') {
                self.advance();
                self.skip_whitespace();
                let val = self.parse_attr_value()?;
                attrs.insert(name, Some(val));
            } else {
                attrs.insert(name, None); // boolean attr
            }
        }
        Ok(attrs)
    }

    fn parse_attr_value(&mut self) -> PResult<String> {
        match self.peek() {
            Some(q @ '"') | Some(q @ '\'') => {
                let quote = q;
                self.advance();
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c == quote {
                        break;
                    }
                    self.pos += c.len_utf8();
                }
                let val = self.src[start..self.pos].to_string();
                self.advance();
                Ok(val)
            }
            _ => Ok(self.consume_while(|c| !c.is_ascii_whitespace() && c != '>' && c != '/')),
        }
    }
}

// ══════════════════════════════════════════════════════════════════
//  CSS Selector engine
// ══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
enum SimpleSelector {
    Tag(String),
    Id(String),
    Class(String),
    AttrExists(String),
    AttrEquals(String, String),
    AttrContainsWord(String, String),
    AttrStartsWith(String, String),
    AttrEndsWith(String, String),
    AttrContains(String, String),
}

#[derive(Debug, Clone)]
struct CompoundSelector {
    parts: Vec<SimpleSelector>,
}

#[derive(Debug, Clone)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug, Clone)]
struct SelectorPart {
    compound: CompoundSelector,
    combinator: Option<Combinator>,
}

#[derive(Debug, Clone)]
struct SelectorSequence {
    parts: Vec<SelectorPart>,
}

#[derive(Debug, Clone)]
struct Selector {
    alternatives: Vec<SelectorSequence>,
}

impl Selector {
    fn parse(selector: &str) -> Self {
        let alternatives = selector
            .split(',')
            .filter_map(|s| parse_selector_sequence(s.trim()))
            .collect();
        Selector { alternatives }
    }

    fn matches(&self, arena: &Arena, id: NodeId) -> bool {
        self.alternatives.iter().any(|seq| seq.matches(arena, id))
    }
}

impl SelectorSequence {
    fn matches(&self, arena: &Arena, id: NodeId) -> bool {
        if self.parts.is_empty() {
            return false;
        }
        let last = self.parts.last().unwrap();
        if !compound_matches(arena, id, &last.compound) {
            return false;
        }
        if self.parts.len() == 1 {
            return true;
        }
        let mut current = id;
        for i in (0..self.parts.len() - 1).rev() {
            let part = &self.parts[i];
            match &part.combinator {
                Some(Combinator::Child) => match arena.get(current).parent {
                    Some(p) => {
                        if !compound_matches(arena, p, &part.compound) {
                            return false;
                        }
                        current = p;
                    }
                    None => return false,
                },
                Some(Combinator::Descendant) | None => {
                    let mut found = false;
                    let mut ancestor = arena.get(current).parent;
                    while let Some(a) = ancestor {
                        if compound_matches(arena, a, &part.compound) {
                            current = a;
                            found = true;
                            break;
                        }
                        ancestor = arena.get(a).parent;
                    }
                    if !found {
                        return false;
                    }
                }
            }
        }
        true
    }
}

fn compound_matches(arena: &Arena, id: NodeId, compound: &CompoundSelector) -> bool {
    compound.parts.iter().all(|s| simple_matches(arena, id, s))
}

fn simple_matches(arena: &Arena, id: NodeId, sel: &SimpleSelector) -> bool {
    match &arena.get(id).data {
        NodeData::Element { tag, attrs } => match sel {
            SimpleSelector::Tag(t) => t == "*" || t == tag,
            SimpleSelector::Id(v) => attrs
                .get("id")
                .and_then(|x| x.as_deref())
                .map(|s| s == v)
                .unwrap_or(false),
            SimpleSelector::Class(c) => attrs
                .get("class")
                .and_then(|x| x.as_deref())
                .map(|s| s.split_ascii_whitespace().any(|cls| cls == c))
                .unwrap_or(false),
            SimpleSelector::AttrExists(a) => attrs.contains_key(a.as_str()),
            SimpleSelector::AttrEquals(a, v) => attrs
                .get(a.as_str())
                .and_then(|x| x.as_deref())
                .map(|s| s == v)
                .unwrap_or(false),
            SimpleSelector::AttrContainsWord(a, v) => attrs
                .get(a.as_str())
                .and_then(|x| x.as_deref())
                .map(|s| s.split_ascii_whitespace().any(|w| w == v))
                .unwrap_or(false),
            SimpleSelector::AttrStartsWith(a, v) => attrs
                .get(a.as_str())
                .and_then(|x| x.as_deref())
                .map(|s| s.starts_with(v.as_str()))
                .unwrap_or(false),
            SimpleSelector::AttrEndsWith(a, v) => attrs
                .get(a.as_str())
                .and_then(|x| x.as_deref())
                .map(|s| s.ends_with(v.as_str()))
                .unwrap_or(false),
            SimpleSelector::AttrContains(a, v) => attrs
                .get(a.as_str())
                .and_then(|x| x.as_deref())
                .map(|s| s.contains(v.as_str()))
                .unwrap_or(false),
        },
        _ => false,
    }
}

// ── selector parser ─────────────────────────────────────────────

fn parse_selector_sequence(s: &str) -> Option<SelectorSequence> {
    let mut parts: Vec<SelectorPart> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let len = chars.len();
    let mut pending: Option<Combinator> = None;

    while i < len {
        let ws_before = chars[i].is_ascii_whitespace();
        while i < len && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        if chars[i] == '>' {
            pending = Some(Combinator::Child);
            i += 1;
            while i < len && chars[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= len {
                break;
            }
        } else if ws_before && !parts.is_empty() {
            pending = Some(Combinator::Descendant);
        }

        let (compound, consumed) = parse_compound(&chars[i..])?;
        i += consumed;
        if let Some(last) = parts.last_mut() {
            last.combinator = pending.take();
        }
        parts.push(SelectorPart { compound, combinator: None });
        pending = None;
    }

    if parts.is_empty() {
        return None;
    }
    Some(SelectorSequence { parts })
}

fn parse_compound(chars: &[char]) -> Option<(CompoundSelector, usize)> {
    let mut simples = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_ascii_whitespace() || c == '>' || c == ',' => break,
            '#' => {
                i += 1;
                let (name, n) = consume_ident(&chars[i..]);
                simples.push(SimpleSelector::Id(name));
                i += n;
            }
            '.' => {
                i += 1;
                let (name, n) = consume_ident(&chars[i..]);
                simples.push(SimpleSelector::Class(name));
                i += n;
            }
            '[' => {
                i += 1;
                let (s, n) = parse_attr_sel(&chars[i..])?;
                simples.push(s);
                i += n;
            }
            '*' => {
                simples.push(SimpleSelector::Tag("*".to_string()));
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let (name, n) = consume_ident(&chars[i..]);
                simples.push(SimpleSelector::Tag(name.to_ascii_lowercase()));
                i += n;
            }
            _ => break,
        }
    }
    if simples.is_empty() {
        return None;
    }
    Some((CompoundSelector { parts: simples }, i))
}

fn parse_attr_sel(chars: &[char]) -> Option<(SimpleSelector, usize)> {
    let mut i = 0;
    let (attr, n) = consume_ident(&chars[i..]);
    i += n;
    while i < chars.len() && chars[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    if chars[i] == ']' {
        return Some((SimpleSelector::AttrExists(attr.to_ascii_lowercase()), i + 1));
    }
    let op_start = i;
    if chars[i] == '=' {
        i += 1;
    } else if i + 1 < chars.len() && chars[i + 1] == '=' {
        i += 2;
    } else {
        return None;
    }
    let op: String = chars[op_start..i].iter().collect();
    while i < chars.len() && chars[i].is_ascii_whitespace() {
        i += 1;
    }
    let val;
    if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
        let quote = chars[i];
        i += 1;
        let start = i;
        while i < chars.len() && chars[i] != quote {
            i += 1;
        }
        val = chars[start..i].iter().collect::<String>();
        if i < chars.len() {
            i += 1;
        }
    } else {
        let (v, n2) = consume_ident(&chars[i..]);
        val = v;
        i += n2;
    }
    while i < chars.len() && chars[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < chars.len() && chars[i] == ']' {
        i += 1;
    }
    let attr = attr.to_ascii_lowercase();
    let s = match op.as_str() {
        "=" => SimpleSelector::AttrEquals(attr, val),
        "~=" => SimpleSelector::AttrContainsWord(attr, val),
        "^=" => SimpleSelector::AttrStartsWith(attr, val),
        "$=" => SimpleSelector::AttrEndsWith(attr, val),
        "*=" => SimpleSelector::AttrContains(attr, val),
        _ => return None,
    };
    Some((s, i))
}

fn consume_ident(chars: &[char]) -> (String, usize) {
    let mut i = 0;
    while i < chars.len()
        && (chars[i].is_alphanumeric()
            || chars[i] == '-'
            || chars[i] == '_'
            || chars[i] == ':'
            || chars[i] == '.')
    {
        i += 1;
    }
    (chars[..i].iter().collect(), i)
}

// ══════════════════════════════════════════════════════════════════
//  Utility helpers
// ══════════════════════════════════════════════════════════════════

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.'
}

fn is_attr_name_char(c: char) -> bool {
    !c.is_ascii_whitespace() && c != '=' && c != '>' && c != '/' && c != '"' && c != '\''
}

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input"
            | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

// ══════════════════════════════════════════════════════════════════
//  napi-rs public bindings
//
//  Design: the Arena is wrapped in Arc<Mutex<Arena>> so that JS
//  Element handles can share it safely.  NodeId is carried as a
//  plain usize inside each JsElement / JsDocument.
// ══════════════════════════════════════════════════════════════════

type SharedArena = Arc<Mutex<Arena>>;

// ── JsElement ───────────────────────────────────────────────────

/// A handle to a single element/document node.
#[napi]
pub struct JsElement {
    arena: SharedArena,
    id: usize, // NodeId index
}

#[napi]
impl JsElement {
    // ── tagName ─────────────────────────────────────────────────

    /// The tag name of the element (lowercase), or null for non-element nodes.
    #[napi(getter)]
    pub fn tag_name(&self) -> Option<String> {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Element { tag, .. } => Some(tag.clone()),
            _ => None,
        }
    }

    // ── nodeType (1 = Element, 3 = Text, 9 = Document) ──────────

    #[napi(getter)]
    pub fn node_type(&self) -> u32 {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Document => 9,
            NodeData::Element { .. } => 1,
            NodeData::Text(_) => 3,
        }
    }

    // ── textContent ─────────────────────────────────────────────

    #[napi(getter)]
    pub fn text_content(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.text_content(NodeId(self.id))
    }

    // ── innerHTML ───────────────────────────────────────────────
    // Exposed as getInnerHtml / setInnerHtml plain methods.
    // The JS wrapper in index.js installs a real Object.defineProperty
    // so that el.innerHTML = "..." and el.innerHTML both work correctly
    // across Node.js and Bun.

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

    // ── outerHTML ───────────────────────────────────────────────

    #[napi(getter)]
    pub fn outer_html(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.outer_html(NodeId(self.id))
    }

    // ── Attribute API ────────────────────────────────────────────

    #[napi]
    pub fn get_attribute(&self, name: String) -> Option<String> {
        let arena = self.arena.lock().unwrap();
        match &arena.get(NodeId(self.id)).data {
            NodeData::Element { attrs, .. } => {
                attrs.get(&name).and_then(|v| v.clone())
            }
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

    /// Pass `value: null` (via JS) to set a boolean attribute with no value.
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
            .map(|&c| JsElement {
                arena: Arc::clone(&self.arena),
                id: c.0,
            })
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
        arena
            .query_selector(NodeId(self.id), &selector)
            .map(|found| JsElement {
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
            .map(|found| JsElement {
                arena: Arc::clone(&self.arena),
                id: found.0,
            })
            .collect()
    }
}

// ── JsDocument ──────────────────────────────────────────────────

/// The parsed document.  Acts like `document` in the browser.
#[napi]
pub struct JsDocument {
    arena: SharedArena,
    root: usize,
}

#[napi]
impl JsDocument {
    /// Parse an HTML-like string and return a Document.
    #[napi(factory)]
    pub fn parse(html: String) -> napi::Result<JsDocument> {
        let mut arena = Arena::new();
        let root = arena.root;
        let mut parser = Parser::new(html);
        parser
            .parse_children(&mut arena, root)
            .map_err(parse_err)?;
        Ok(JsDocument {
            arena: Arc::new(Mutex::new(arena)),
            root: root.0,
        })
    }

    /// The document root element (like `document.documentElement`).
    #[napi(getter)]
    pub fn document_element(&self) -> JsElement {
        JsElement {
            arena: Arc::clone(&self.arena),
            id: self.root,
        }
    }

    /// Shortcut: querySelector from the document root.
    #[napi]
    pub fn query_selector(&self, selector: String) -> Option<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena
            .query_selector(NodeId(self.root), &selector)
            .map(|found| JsElement {
                arena: Arc::clone(&self.arena),
                id: found.0,
            })
    }

    /// Shortcut: querySelectorAll from the document root.
    #[napi]
    pub fn query_selector_all(&self, selector: String) -> Vec<JsElement> {
        let arena = self.arena.lock().unwrap();
        arena
            .query_selector_all(NodeId(self.root), &selector)
            .into_iter()
            .map(|found| JsElement {
                arena: Arc::clone(&self.arena),
                id: found.0,
            })
            .collect()
    }

    /// Serialise the entire document back to a string.
    #[napi]
    pub fn serialize(&self) -> String {
        let arena = self.arena.lock().unwrap();
        arena.inner_html(NodeId(self.root))
    }
}