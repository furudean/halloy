use std::collections::{HashMap, HashSet};

use crate::target::{self, Target};
use crate::{Server, client, history};

#[derive(Debug, Default)]
struct Buffers {
    channels: HashSet<target::Channel>,
    queries: HashSet<target::Query>,
}

// Tracks the channels and queries shown in the sidebar, so they are saved
// with the dashboard and can be restored on the next launch.
#[derive(Debug, Default)]
pub struct SavedBuffers {
    buffers: HashMap<Server, Buffers>,
    // Holds restored channels that are not joined yet. Restoring a channel
    // does not join it.
    restored_channels: HashMap<Server, HashSet<target::Channel>>,
    // Holds the channels joined on each server's current connection.
    joined: HashMap<Server, HashSet<target::Channel>>,
    changed: bool,
    exited: bool,
}

impl SavedBuffers {
    pub fn restore(&mut self, server: Server, target: Target) {
        let buffers = self.buffers.entry(server.clone()).or_default();

        match target {
            Target::Channel(channel) => {
                buffers.channels.insert(channel.clone());

                self.restored_channels
                    .entry(server)
                    .or_default()
                    .insert(channel);
            }
            Target::Query(query) => {
                buffers.queries.insert(query);
            }
        }
    }

    pub fn restored_channels(
        &self,
        server: &Server,
    ) -> impl Iterator<Item = &target::Channel> {
        self.restored_channels.get(server).into_iter().flatten()
    }

    pub fn close(&mut self, kind: &history::Kind) {
        if let history::Kind::Channel(server, channel) = kind {
            if let Some(channels) = self.restored_channels.get_mut(server) {
                channels.remove(channel);
            }

            if let Some(buffers) = self.buffers.get_mut(server) {
                self.changed |= buffers.channels.remove(channel);
            }

            // A rejected PART leaves the channel joined, so it is saved again
            // on the next update.
            if let Some(joined) = self.joined.get_mut(server) {
                joined.remove(channel);
            }
        }
    }

    // Resets join tracking, so a reconnect is never mistaken for parts.
    pub fn disconnected(&mut self, server: &Server) {
        self.joined.remove(server);
    }

    // Sidebar buffers also change through incoming messages and joins, so they
    // are compared on each call instead of marked at every change.
    //
    // A channel leaves the saved buffers when it is closed or parted. A
    // disconnect does not part channels, so they stay saved across reconnects.
    // Saved channels that a new connection has not joined are shown as
    // restored channels, so the sidebar matches what the next launch restores.
    //
    // Nothing is tracked while restore_on_launch is disabled, which a config
    // reload can change. Channels of bouncer networks are skipped, since the
    // bouncer rejoins them.
    //
    // Returns true when the saved buffers changed.
    pub fn update<'a>(
        &mut self,
        queries: impl Iterator<Item = (&'a Server, &'a target::Query)> + Clone,
        clients: &client::Map,
        restore_on_launch: bool,
    ) -> bool {
        // History is empty once exit starts, so the exit snapshot stays.
        if self.exited {
            return false;
        }

        if !restore_on_launch {
            let cleared = self.buffers.values().any(|buffers| {
                !buffers.channels.is_empty() || !buffers.queries.is_empty()
            });

            self.buffers.clear();
            self.restored_channels.clear();
            self.joined.clear();
            self.changed = false;

            return cleared;
        }

        let mut changed = std::mem::take(&mut self.changed);

        // Joined channels are tracked by their client from here on.
        for (server, channels) in &mut self.restored_channels {
            channels
                .retain(|channel| !clients.contains_channel(server, channel));
        }

        self.joined
            .retain(|server, _| clients.get_server_is_connected(server));

        for server in clients
            .connected_servers()
            .filter(|server| !server.is_bouncer_network())
        {
            if !self.joined.contains_key(server)
                && let Some(buffers) = self.buffers.get(server)
            {
                self.restored_channels
                    .entry(server.clone())
                    .or_default()
                    .extend(
                        buffers
                            .channels
                            .iter()
                            .filter(|channel| {
                                !clients.contains_channel(server, channel)
                            })
                            .cloned(),
                    );
            }

            let joined = self.joined.entry(server.clone()).or_default();
            let saved =
                &mut self.buffers.entry(server.clone()).or_default().channels;

            joined.retain(|channel| {
                let parted = !clients.contains_channel(server, channel);

                if parted {
                    changed |= saved.remove(channel);
                }

                !parted
            });

            for channel in clients.get_channels(server) {
                if !joined.contains(channel) {
                    joined.insert(channel.clone());
                    changed |= saved.insert(channel.clone());
                }
            }
        }

        self.restored_channels
            .retain(|_, channels| !channels.is_empty());

        if !self.queries_unchanged(queries.clone()) {
            for buffers in self.buffers.values_mut() {
                buffers.queries.clear();
            }

            for (server, query) in queries {
                self.buffers
                    .entry(server.clone())
                    .or_default()
                    .queries
                    .insert(query.clone());
            }

            changed = true;
        }

        changed
    }

    fn queries_unchanged<'a>(
        &self,
        mut queries: impl Iterator<Item = (&'a Server, &'a target::Query)>,
    ) -> bool {
        let mut count = 0;

        queries.all(|(server, query)| {
            count += 1;

            self.buffers
                .get(server)
                .is_some_and(|buffers| buffers.queries.contains(query))
        }) && count
            == self
                .buffers
                .values()
                .map(|buffers| buffers.queries.len())
                .sum::<usize>()
    }

    pub fn exit(&mut self) {
        self.exited = true;
    }

    pub fn to_vec(&self) -> Vec<(Server, Target)> {
        self.buffers
            .iter()
            .flat_map(|(server, buffers)| {
                buffers
                    .channels
                    .iter()
                    .cloned()
                    .map(Target::Channel)
                    .chain(buffers.queries.iter().cloned().map(Target::Query))
                    .map(|target| (server.clone(), target))
            })
            .collect()
    }
}
