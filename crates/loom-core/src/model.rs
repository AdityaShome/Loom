use serde::{Deserialize, Serialize};

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($v:ident),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name { $($v),* }
        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$v => stringify!($v)),* }
            }
            pub fn parse(s: &str) -> Option<Self> {
                $(if s == stringify!($v) { return Some(Self::$v); })*
                None
            }
        }
    };
}

str_enum!(Provider { Anthropic, OpenAi });
str_enum!(NodeKind { System, User, Assistant, ToolCall, ToolResult, File, Memory, Summary });
str_enum!(
    /// Role as sent to the provider API.
    Role { System, User, Assistant, Tool }
);
str_enum!(
    /// Edge direction: ToolPair call -> result; Follows later -> earlier;
    /// Supersedes newer -> older; RefersTo referrer -> referenced.
    EdgeKind { ToolPair, Follows, RefersTo, Supersedes, SummaryOf, DerivedFrom }
);
str_enum!(
    /// What to do with a node when building the next request.
    Action { Keep, Summarize, Drop }
);
str_enum!(
    /// What kind of thing a knowledge-graph entity is. Concept and Note are for things you add yourself.
    EntityKind { File, Symbol, Command, Error, Goal, Concept, Note }
);
str_enum!(Status { Full, Summarized, Dropped, DroppedByAgent });

/// Where a node came from in the request body, so the rewriter can edit it in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    /// The top-level Anthropic `system` field.
    System,
    /// `messages[msg]`; `block` is the index within its content blocks
    /// (Anthropic) or `tool_calls` (OpenAI), `None` for the message's own content.
    Msg { msg: usize, block: Option<usize> },
}

/// One context item in provider-neutral form, as produced by `normalize`.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonMsg {
    pub role: Role,
    pub kind: NodeKind,
    /// Normalized content: trimmed text, or canonical JSON for tool input.
    pub content: String,
    /// Tool name, file path, or memory key.
    pub source: Option<String>,
    /// `tool_use.id` / `tool_use_id` (Anthropic) or `tool_call_id` (OpenAI).
    pub tool_id: Option<String>,
    /// Position in the request body (not part of the content hash).
    pub loc: Loc,
    /// A block type Loom cannot safely edit (thinking, images, ...): always kept as is.
    pub opaque: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: String,
    pub session_id: String,
    #[serde(skip)]
    pub content_hash: [u8; 32],
    /// Nth occurrence of this hash within a request (distinguishes repeated messages).
    pub occ: u32,
    pub kind: NodeKind,
    pub role: Role,
    pub content: String,
    pub tokens: u32,
    pub first_seen_turn: u32,
    pub last_seen_turn: u32,
    pub position: u32,
    pub source: Option<String>,
    pub importance: f32,
    pub pinned: bool,
    /// Cannot be rewritten (see `CanonMsg::opaque`).
    pub locked: bool,
    /// The user replaced the text the model sees for this node.
    pub edited: bool,
    pub user_action: Option<String>,
    pub status: Status,
    pub summary_node: Option<String>,
    pub score: f32,
}
