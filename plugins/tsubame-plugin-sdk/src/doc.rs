use crate::{Button, Image, Node};
use alloc::string::String;
use alloc::vec::Vec;

/// Builds the rows a previewer returns. Every method adds one row and returns
/// the builder, so calls chain; [`Doc::finish`] gives the `Vec<Node>`.
///
/// ```
/// use tsubame_plugin_sdk::Doc;
/// let nodes = Doc::new().heading("report.csv").pair("Rows", "120").finish();
/// assert_eq!(nodes.len(), 2);
/// ```
#[derive(Default)]
pub struct Doc {
    nodes: Vec<Node>,
}

impl Doc {
    /// An empty document.
    pub fn new() -> Doc {
        Doc::default()
    }

    /// A heading.
    pub fn heading(&mut self, text: impl Into<String>) -> &mut Doc {
        self.push(Node::Heading(text.into()))
    }

    /// Plain text.
    pub fn text(&mut self, text: impl Into<String>) -> &mut Doc {
        self.push(Node::Text(text.into()))
    }

    /// Text in the secondary colour.
    pub fn dim(&mut self, text: impl Into<String>) -> &mut Doc {
        self.push(Node::DimText(text.into()))
    }

    /// Monospace text (code, hex).
    pub fn mono(&mut self, text: impl Into<String>) -> &mut Doc {
        self.push(Node::Mono(text.into()))
    }

    /// A label and a value.
    pub fn pair(&mut self, label: impl Into<String>, value: impl Into<String>) -> &mut Doc {
        self.push(Node::Pair((label.into(), value.into())))
    }

    /// An image of premultiplied BGRA pixels, `width * height * 4` bytes, top
    /// row first. The host rejects any other size, and a side over the
    /// caller's `limits.max_px`.
    pub fn image(&mut self, width: u32, height: u32, bgra: Vec<u8>) -> &mut Doc {
        self.push(Node::Image(Image { width, height, bgra }))
    }

    /// A button that runs the registered command `command` when pressed.
    pub fn button(&mut self, label: impl Into<String>, command: impl Into<String>) -> &mut Doc {
        self.push(Node::Button(Button { label: label.into(), command: command.into() }))
    }

    /// Indents the rows that follow by one step, until [`Doc::outdent`].
    pub fn indent(&mut self) -> &mut Doc {
        self.push(Node::Indent)
    }

    /// Ends one level of [`Doc::indent`].
    pub fn outdent(&mut self) -> &mut Doc {
        self.push(Node::Outdent)
    }

    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when there are no rows.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The rows.
    pub fn finish(&mut self) -> Vec<Node> {
        core::mem::take(&mut self.nodes)
    }

    fn push(&mut self, n: Node) -> &mut Doc {
        self.nodes.push(n);
        self
    }
}
