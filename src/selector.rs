use crate::arena::{Arena, NodeData, NodeId};

// ── Simple selector types ────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum SimpleSelector {
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
pub(crate) struct CompoundSelector {
    pub parts: Vec<SimpleSelector>,
}

#[derive(Debug, Clone)]
pub(crate) enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug, Clone)]
pub(crate) struct SelectorPart {
    pub compound: CompoundSelector,
    pub combinator: Option<Combinator>,
}

#[derive(Debug, Clone)]
pub(crate) struct SelectorSequence {
    pub parts: Vec<SelectorPart>,
}

#[derive(Debug, Clone)]
pub(crate) struct Selector {
    alternatives: Vec<SelectorSequence>,
}

// ── Selector entry point ─────────────────────────────────────────

impl Selector {
    pub fn parse(selector: &str) -> Self {
        let alternatives = selector
            .split(',')
            .filter_map(|s| parse_selector_sequence(s.trim()))
            .collect();
        Selector { alternatives }
    }

    pub fn matches(&self, arena: &Arena, id: NodeId) -> bool {
        self.alternatives.iter().any(|seq| seq.matches(arena, id))
    }
}

// ── Matching ─────────────────────────────────────────────────────

impl SelectorSequence {
    pub fn matches(&self, arena: &Arena, id: NodeId) -> bool {
        if self.parts.is_empty() {
            return false;
        }
        // Match right-to-left: last part must match the target node.
        if !compound_matches(arena, id, &self.parts.last().unwrap().compound) {
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

// ── Selector parser ──────────────────────────────────────────────

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

// ── Identifier lexer (shared by tag, id, class, attr names) ─────
// Allows: alphanumeric, hyphen, underscore, colon, dot
// so that n:route, xml:lang, data-x.y all parse correctly.

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