//! A tiny YAML writer for generated compose files.
//!
//! serde_yaml cannot emit comments, and the override file explains each
//! line it adds ("removed because postgres is shared"). The node tree here
//! covers exactly what an override needs: maps, lists, strings, tags.
//! Strings are always written as JSON strings, which are valid YAML
//! double-quoted scalars, so no value can break the structure.

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Map(Vec<Entry>),
    Seq(Vec<Node>),
    Str(String),
    /// Written as-is: numbers and booleans.
    Raw(String),
    Null,
    /// `!reset` / `!override` applied to a value.
    Tagged(&'static str, Box<Node>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Node,
    pub comment: Option<String>,
}

impl Node {
    /// Convert a JSON value (as used by compose edits) into a node.
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Node::Null,
            serde_json::Value::Bool(b) => Node::Raw(b.to_string()),
            serde_json::Value::Number(n) => Node::Raw(n.to_string()),
            serde_json::Value::String(s) => Node::Str(s.clone()),
            serde_json::Value::Array(a) => Node::Seq(a.iter().map(Node::from_json).collect()),
            serde_json::Value::Object(o) => Node::Map(
                o.iter()
                    .map(|(k, v)| Entry {
                        key: k.clone(),
                        value: Node::from_json(v),
                        comment: None,
                    })
                    .collect(),
            ),
        }
    }

    pub fn map() -> Self {
        Node::Map(Vec::new())
    }

    pub fn str(s: impl Into<String>) -> Self {
        Node::Str(s.into())
    }

    pub fn tagged(tag: &'static str, value: Node) -> Self {
        Node::Tagged(tag, Box::new(value))
    }

    /// Append `key: value` to a map node. Panics on non-maps (programming error).
    pub fn push(&mut self, key: impl Into<String>, value: Node, comment: Option<&str>) {
        match self {
            Node::Map(entries) => entries.push(Entry {
                key: key.into(),
                value,
                comment: comment.map(str::to_string),
            }),
            _ => panic!("push on a non-map YAML node"),
        }
    }

    /// Get or create a nested map under `key`.
    pub fn child(&mut self, key: &str) -> &mut Node {
        let Node::Map(entries) = self else {
            panic!("child on a non-map YAML node")
        };
        let idx = match entries.iter().position(|e| e.key == key) {
            Some(i) => i,
            None => {
                entries.push(Entry {
                    key: key.to_string(),
                    value: Node::map(),
                    comment: None,
                });
                entries.len() - 1
            }
        };
        &mut entries[idx].value
    }

    pub fn is_empty_map(&self) -> bool {
        matches!(self, Node::Map(e) if e.is_empty())
    }
}

/// Render a document. `header` lines become leading comments.
pub fn render(root: &Node, header: &[&str]) -> String {
    let mut out = String::new();
    for line in header {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    if let Node::Map(entries) = root {
        write_entries(&mut out, entries, 0);
    } else {
        write_inline_or_block(&mut out, root, 0, None);
    }
    out
}

fn write_entries(out: &mut String, entries: &[Entry], indent: usize) {
    for e in entries {
        pad(out, indent);
        out.push_str(&key(&e.key));
        out.push(':');
        write_inline_or_block(out, &e.value, indent, e.comment.as_deref());
    }
}

/// Write a value after `key:` (or at the top level): scalars and empty
/// collections stay on the line, others open a block below.
fn write_inline_or_block(out: &mut String, node: &Node, indent: usize, comment: Option<&str>) {
    let (tag, inner) = match node {
        Node::Tagged(t, inner) => (Some(*t), inner.as_ref()),
        other => (None, other),
    };
    if let Some(t) = tag {
        out.push(' ');
        out.push_str(t);
    }
    match inner {
        Node::Str(s) => {
            out.push(' ');
            out.push_str(&quote(s));
            end_line(out, comment);
        }
        Node::Raw(r) => {
            out.push(' ');
            out.push_str(r);
            end_line(out, comment);
        }
        Node::Null => {
            out.push_str(" null");
            end_line(out, comment);
        }
        Node::Map(entries) if entries.is_empty() => {
            out.push_str(" {}");
            end_line(out, comment);
        }
        Node::Seq(items) if items.is_empty() => {
            out.push_str(" []");
            end_line(out, comment);
        }
        Node::Map(entries) => {
            end_line(out, comment);
            write_entries(out, entries, indent + 2);
        }
        Node::Seq(items) => {
            end_line(out, comment);
            for item in items {
                pad(out, indent + 2);
                out.push('-');
                match item {
                    Node::Map(entries) if !entries.is_empty() => {
                        // `- key: v` with the rest aligned under the first key.
                        out.push(' ');
                        let mut first = true;
                        for e in entries {
                            if !first {
                                pad(out, indent + 4);
                            }
                            first = false;
                            out.push_str(&key(&e.key));
                            out.push(':');
                            write_inline_or_block(out, &e.value, indent + 4, e.comment.as_deref());
                        }
                    }
                    other => write_inline_or_block(out, other, indent + 2, None),
                }
            }
        }
        Node::Tagged(..) => unreachable!("nested tags are not produced"),
    }
}

fn end_line(out: &mut String, comment: Option<&str>) {
    if let Some(c) = comment {
        out.push_str("  # ");
        out.push_str(c);
    }
    out.push('\n');
}

fn pad(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Plain keys for the common case, quoted otherwise.
fn key(k: &str) -> String {
    let plain = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !k.starts_with(['-', '.']);
    if plain { k.to_string() } else { quote(k) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tags_comments_and_nesting() {
        let mut root = Node::map();
        let api = root.child("services").child("api");
        api.push("image", Node::str("myapp-feat-api"), Some("own tag"));
        api.push("container_name", Node::tagged("!reset", Node::Null), None);
        api.push(
            "ports",
            Node::tagged("!override", Node::Seq(vec![Node::str("127.0.0.1:41000:9229")])),
            None,
        );
        let mut deps = Node::map();
        let mut cond = Node::map();
        cond.push("condition", Node::str("service_started"), None);
        deps.push("worker", cond, None);
        api.push("depends_on", Node::tagged("!override", deps), None);
        root.child("services").child("web").push(
            "ports",
            Node::tagged("!reset", Node::Seq(vec![])),
            Some("via gateway"),
        );

        let text = render(&root, &["generated"]);
        let expected = "# generated\n\
services:\n  \
  api:\n    \
    image: \"myapp-feat-api\"  # own tag\n    \
    container_name: !reset null\n    \
    ports: !override\n      \
      - \"127.0.0.1:41000:9229\"\n    \
    depends_on: !override\n      \
      worker:\n        \
        condition: \"service_started\"\n  \
  web:\n    \
    ports: !reset []  # via gateway\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn quotes_awkward_strings_and_keys() {
        let mut root = Node::map();
        root.push("a key", Node::str("x: \"y\"\n#z"), None);
        let text = render(&root, &[]);
        assert_eq!(text, "\"a key\": \"x: \\\"y\\\"\\n#z\"\n");
        // Round-trips through a real YAML parser.
        let v: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(v["a key"].as_str(), Some("x: \"y\"\n#z"));
    }

    #[test]
    fn seq_of_maps() {
        let mut root = Node::map();
        let mut item = Node::map();
        item.push("a", Node::str("1"), None);
        item.push("b", Node::str("2"), None);
        root.push("list", Node::Seq(vec![item]), None);
        let text = render(&root, &[]);
        assert_eq!(text, "list:\n  - a: \"1\"\n    b: \"2\"\n");
        let v: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(v["list"][0]["b"].as_str(), Some("2"));
    }
}
