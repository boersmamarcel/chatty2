use std::path::PathBuf;
use std::time::Duration;

use chatty_core::models::message_types::{ApprovalBlock, ThinkingBlock, ToolCallBlock};

/// Stable identifier for a transcript block. Never a list index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u64);

impl BlockId {
    pub fn from_parts(namespace: u64, key: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        namespace.hash(&mut hasher);
        key.hash(&mut hasher);
        Self(hasher.finish())
    }

    pub fn element_id(self) -> gpui::ElementId {
        gpui::ElementId::NamedInteger("block".into(), self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
}

/// Typed transcript block. Lives in chatty-gpui only.
#[derive(Clone, Debug)]
pub enum Block {
    User {
        id: BlockId,
        content: String,
        attachments: Vec<PathBuf>,
    },
    Text {
        id: BlockId,
        content: String,
        streaming: bool,
    },
    Thinking {
        id: BlockId,
        block: ThinkingBlock,
    },
    Activity {
        id: BlockId,
        tools: Vec<ToolCallBlock>,
    },
    Diff {
        id: BlockId,
        tool: ToolCallBlock,
    },
    Approval {
        id: BlockId,
        approval: ApprovalBlock,
    },
    Plan {
        id: BlockId,
    },
    Artifact {
        id: BlockId,
        path: PathBuf,
    },
    Error {
        id: BlockId,
        message: String,
        detail: Option<String>,
    },
}

impl Block {
    pub fn id(&self) -> BlockId {
        match self {
            Self::User { id, .. }
            | Self::Text { id, .. }
            | Self::Thinking { id, .. }
            | Self::Activity { id, .. }
            | Self::Diff { id, .. }
            | Self::Approval { id, .. }
            | Self::Plan { id }
            | Self::Artifact { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }
}

/// One display message mapped onto an ordered list of blocks.
#[derive(Clone, Debug)]
pub struct Turn {
    pub id: u64,
    pub message_index: usize,
    pub role: TurnRole,
    pub blocks: Vec<Block>,
    pub elapsed: Option<Duration>,
    pub collapsed: bool,
    pub streaming: bool,
}

impl Turn {
    pub fn first_block_id(&self) -> Option<BlockId> {
        self.blocks.first().map(Block::id)
    }
}
