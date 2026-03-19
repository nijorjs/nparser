use std::collections::HashMap;
use std::fmt;
use crate::arena::{Arena, NodeData, NodeId};

// ── Error type ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ParseError {
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

// Convenience alias — keeps internal signatures free of napi's Result type.
pub(crate) type PResult<T> = std::result::Result<T, ParseError>;

pub(crate) fn parse_err(e: ParseError) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, e.to_string())
}

// ── Character helpers ────────────────────────────────────────────

pub(crate) fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.'
}

fn is_attr_name_char(c: char) -> bool {
    !c.is_ascii_whitespace() && c != '=' && c != '>' && c != '/' && c != '"' && c != '\''
}

pub(crate) fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input"
            | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

// ── Parser ───────────────────────────────────────────────────────

pub(crate) struct Parser {
    src: String,
    pos: usize,
}

impl Parser {
    pub fn new(src: impl Into<String>) -> Self {
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

    // ── Recursive descent ────────────────────────────────────────

    pub fn parse_children(
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

        // Void element — no children, no closing tag
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