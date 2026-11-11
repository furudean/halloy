use jiff::Timestamp;

use super::Id;
use crate::message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Older,
    Newer,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Self::Older => Self::Newer,
            Self::Newer => Self::Older,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub side: Side,
    pub history_id: Id,
    pub time: Timestamp,
    pub references: message::MessageReferences,
    pub across: Option<Timestamp>,
    pub across_references: Option<message::MessageReferences>,
}

/// The gaps at either end of a stretch.
#[derive(Debug, Clone, Default)]
pub struct Edges {
    pub older: Option<Edge>,
    pub newer: Option<Edge>,
}

#[derive(Debug, Clone, Copy)]
pub enum Fill {
    Missing(Timestamp),
    Complete {
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    },
}
