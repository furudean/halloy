use serde::Deserialize;

use crate::metadata;
use crate::metadata::Key::{self, *};

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    /// List of keys to subscribe to, in order of preference
    pub preferred_keys: Vec<metadata::Key>,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            preferred_keys: vec![
                DisplayName,
                Avatar,
                Pronouns,
                Homepage,
                Color,
                Status,
            ],
        }
    }
}

impl Metadata {
    pub fn preferred_key_strs(&self) -> impl Iterator<Item = &'static str> {
        self.preferred_keys.iter().copied().map(Key::to_str)
    }
}
