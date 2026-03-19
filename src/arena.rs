use std::collections::HashMap;
use crate::selector::Selector;
use crate::parser::is_void;

// ── Node identity ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodeId(pub usize);

// ── Node data ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum NodeData {
    Document,
    Element {
        tag: String,
        attrs: HashMap<String, Option<String>>,
    },
    Text(String),
}

// ── Node ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct InternalNode {
    pub data: NodeData,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

// ── Arena ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Arena {
    pub nodes: Vec<InternalNode>,
    pub root: NodeId,
}

impl Arena {
    pub fn new() -> Self {
        Arena {
            nodes: vec![InternalNode {
                data: NodeData::Document,
                parent: None,
                children: vec![],
            }],
            root: NodeId(0),
        }
    }

    pub fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(InternalNode {
            data,
            parent: None,
            children: vec![],
        });
        id
    }

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[child.0].parent = Some(parent);
        self.nodes[parent.0].children.push(child);
    }

    pub fn get(&self, id: NodeId) -> &InternalNode {
        &self.nodes[id.0]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut InternalNode {
        &mut self.nodes[id.0]
    }

    pub fn clear_children(&mut self, parent: NodeId) {
        let children = std::mem::take(&mut self.nodes[parent.0].children);
        for child in children {
            self.nodes[child.0].parent = None;
            self.clear_children(child);
        }
    }

    // ── Serialisation ────────────────────────────────────────────

    pub fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for &child in &self.nodes[id.0].children {
            self.serialise_node(child, &mut out);
        }
        out
    }

    pub fn outer_html(&self, id: NodeId) -> String {
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

    // ── Text content ─────────────────────────────────────────────

    pub fn text_content(&self, id: NodeId) -> String {
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

    // ── querySelector ────────────────────────────────────────────

    pub fn query_selector(&self, start: NodeId, selector: &str) -> Option<NodeId> {
        let sel = Selector::parse(selector);
        let mut result = None;
        self.walk_descendants(start, &mut |id| {
            if result.is_none() && sel.matches(self, id) {
                result = Some(id);
            }
        });
        result
    }

    pub fn query_selector_all(&self, start: NodeId, selector: &str) -> Vec<NodeId> {
        let sel = Selector::parse(selector);
        let mut results = Vec::new();
        self.walk_descendants(start, &mut |id| {
            if sel.matches(self, id) {
                results.push(id);
            }
        });
        results
    }

    pub fn walk_descendants<F: FnMut(NodeId)>(&self, id: NodeId, f: &mut F) {
        for &child in &self.nodes[id.0].children {
            f(child);
            self.walk_descendants(child, f);
        }
    }
}