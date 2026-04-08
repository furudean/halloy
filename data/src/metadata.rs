use std::collections::HashMap;
use std::str::FromStr;

use iced::Color;

use crate::Url;
use crate::target::{Channel, Query, Target, TargetRef};

type Metadata = HashMap<String, String>;

pub trait Registry {
    fn get_user(&self, target: &Query, key: &str) -> Option<&str>;
    fn get_channel(&self, target: &Channel, key: &str) -> Option<&str>;

    fn get(&self, target: TargetRef<'_>, key: &str) -> Option<&str> {
        match target {
            TargetRef::Channel(channel) => self.get_channel(channel, key),
            TargetRef::Query(query) => self.get_user(query, key),
        }
    }

    // channel or user metadata
    // https://ircv3.net/registry.html#channel-metadata
    fn avatar(&self, target: TargetRef<'_>) -> Option<&str> {
        self.get(target, "avatar")
    }
    fn display_name(&self, target: TargetRef<'_>) -> Option<&str> {
        self.get(target, "display-name")
    }

    // user metadata
    // https://ircv3.net/registry.html#user-metadata
    fn color(&self, target: &Query) -> Option<Color> {
        self.get_user(target, "color")
            .and_then(|s| Color::from_str(s).ok())
    }
    fn homepage(&self, target: &Query) -> Option<Url> {
        self.get_user(target, "homepage")
            .and_then(|s| Url::from_str(s).ok())
    }
    fn status(&self, target: &Query) -> Option<&str> {
        self.get_user(target, "status, target: &Query")
    }
    fn pronouns(&self, target: &Query) -> Option<&str> {
        self.get_user(target, "pronouns")
    }
}

#[derive(Debug, Default)]
pub struct ServerRegistry {
    channels: HashMap<Channel, Metadata>,
    users: HashMap<Query, Metadata>,
}

impl ServerRegistry {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            users: HashMap::new(),
        }
    }

    pub fn insert(&mut self, target: Target, key: String, value: String) {
        match target {
            Target::Channel(channel) => {
                self.channels.entry(channel).or_default().insert(key, value);
            }
            Target::Query(query) => {
                self.users.entry(query).or_default().insert(key, value);
            }
        }
    }
}

impl Registry for ServerRegistry {
    fn get_user(&self, target: &Query, key: &str) -> Option<&str> {
        self.users
            .get(target)
            .and_then(|r| r.get(key))
            .map(String::as_str)
    }

    fn get_channel(&self, target: &Channel, key: &str) -> Option<&str> {
        self.channels
            .get(target)
            .and_then(|r| r.get(key))
            .map(String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct EmptyRegistry();
impl Registry for EmptyRegistry {
    fn get_user(&self, _target: &Query, _key: &str) -> Option<&str> {
        None
    }

    fn get_channel(&self, _target: &Channel, _key: &str) -> Option<&str> {
        None
    }
}

pub static EMPTY: &EmptyRegistry = &EmptyRegistry();
