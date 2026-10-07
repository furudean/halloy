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
pub struct SidebarBuffers {
    buffers: HashMap<Server, Buffers>,
    // Holds the channels shown in the sidebar while disconnected from them,
    // whether restored (restoring a channel does not join it), left, or
    // dropped with their server's connection.
    disconnected: HashMap<Server, HashSet<target::Channel>>,
    // Holds the channels joined on each server's current connection.
    joined: HashMap<Server, HashSet<target::Channel>>,
    closing: HashMap<Server, HashSet<target::Channel>>,
    changed: bool,
    exited: bool,
}

impl SidebarBuffers {
    pub fn restore(&mut self, server: Server, target: Target) {
        let buffers = self.buffers.entry(server.clone()).or_default();

        match target {
            Target::Channel(channel) => {
                buffers.channels.insert(channel.clone());

                self.disconnected.entry(server).or_default().insert(channel);
            }
            Target::Query(query) => {
                buffers.queries.insert(query);
            }
        }
    }

    pub fn disconnected_channels(
        &self,
        server: &Server,
    ) -> impl Iterator<Item = &target::Channel> {
        self.disconnected.get(server).into_iter().flatten()
    }

    pub fn left(&mut self, server: Server, channel: target::Channel) {
        if self
            .closing
            .get(&server)
            .is_some_and(|channels| channels.contains(&channel))
        {
            return;
        }

        self.disconnected.entry(server).or_default().insert(channel);
    }

    pub fn close(&mut self, kind: &history::Kind) {
        if let history::Kind::Channel(server, channel) = kind {
            if let Some(channels) = self.disconnected.get_mut(server) {
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

            self.closing
                .entry(server.clone())
                .or_default()
                .insert(channel.clone());
        }
    }

    // Marks the server's joined channels as disconnected, so they stay in the
    // sidebar, and resets join tracking, so a reconnect is never mistaken for
    // parts. Must be called before the server's client is dropped.
    pub fn server_disconnected(
        &mut self,
        server: &Server,
        clients: &client::Map,
    ) {
        let closing = self.closing.remove(server);

        let channels = clients
            .get_channels(server)
            .filter(|channel| {
                !closing
                    .as_ref()
                    .is_some_and(|closing| closing.contains(*channel))
            })
            .cloned()
            .collect::<Vec<_>>();

        if !channels.is_empty() {
            self.disconnected
                .entry(server.clone())
                .or_default()
                .extend(channels);
        }

        self.joined.remove(server);
    }

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

        // Joined channels are tracked by their client from here on.
        for (server, channels) in &mut self.disconnected {
            channels
                .retain(|channel| !clients.contains_channel(server, channel));
        }

        self.disconnected.retain(|_, channels| !channels.is_empty());

        if !restore_on_launch {
            let cleared = self.buffers.values().any(|buffers| {
                !buffers.channels.is_empty() || !buffers.queries.is_empty()
            });

            self.buffers.clear();
            self.joined.clear();
            self.changed = false;
            self.prune_closing(clients);

            return cleared;
        }

        let mut changed = std::mem::take(&mut self.changed);

        self.joined
            .retain(|server, _| clients.get_server_is_connected(server));

        for server in clients
            .connected_servers()
            .filter(|server| !server.is_bouncer_network())
        {
            let joined = self.joined.entry(server.clone()).or_default();
            let saved =
                &mut self.buffers.entry(server.clone()).or_default().channels;
            let closing = self.closing.get(server);

            joined.retain(|channel| {
                let parted = !clients.contains_channel(server, channel);

                // Left channels stay saved, since their buffer stays open.
                if parted
                    && closing.is_some_and(|closing| closing.contains(channel))
                {
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

        self.prune_closing(clients);

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

    fn prune_closing(&mut self, clients: &client::Map) {
        for (server, channels) in &mut self.closing {
            channels
                .retain(|channel| clients.contains_channel(server, channel));
        }

        self.closing.retain(|_, channels| !channels.is_empty());
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use futures::channel::mpsc;
    use irc::proto::{self, Command};

    use super::*;
    use crate::{config, isupport, message};

    fn server() -> Server {
        Server::from(Arc::<str>::from("test"))
    }

    fn channel(name: &str) -> target::Channel {
        target::Channel::parse(
            name,
            isupport::DEFAULT_CHANTYPES,
            &[],
            isupport::CaseMap::default(),
        )
        .unwrap()
    }

    fn connect(clients: &mut client::Map) {
        let (sender, _) = mpsc::channel(1);

        clients.ready(
            server(),
            client::Client::new(
                server(),
                Arc::new(config::Server {
                    nickname: "tester".to_string(),
                    ..Default::default()
                }),
                sender,
            ),
        );
    }

    fn clients() -> client::Map {
        let mut clients = client::Map::default();

        connect(&mut clients);

        clients
    }

    // Mirrors the dashboard, which marks left channels as the events arrive.
    fn receive(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        nickname: &str,
        command: Command,
    ) {
        let events = clients
            .receive(
                &server(),
                message::Encoded(proto::Message {
                    tags: BTreeMap::default(),
                    source: Some(proto::Source::User(proto::User {
                        nickname: nickname.to_string(),
                        username: Some(nickname.to_string()),
                        hostname: Some("example.test".to_string()),
                    })),
                    command,
                }),
                &config::Config::default(),
            )
            .unwrap();

        for event in events {
            if let client::Event::LeftChannel(channel) = event {
                buffers.left(server(), channel);
            }
        }
    }

    fn join(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "tester",
            Command::JOIN(name.to_string(), None),
        );
    }

    fn part(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "tester",
            Command::PART(name.to_string(), None),
        );
    }

    fn kick(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "alice",
            Command::KICK(name.to_string(), "tester".to_string(), None),
        );
    }

    // Mirrors the app, which marks the server disconnected before dropping
    // its client.
    fn disconnect(clients: &mut client::Map, buffers: &mut SidebarBuffers) {
        buffers.server_disconnected(&server(), clients);
        clients.disconnected(server(), false);
    }

    fn update(buffers: &mut SidebarBuffers, clients: &client::Map) -> bool {
        buffers.update(std::iter::empty(), clients, true)
    }

    fn update_disabled(
        buffers: &mut SidebarBuffers,
        clients: &client::Map,
    ) -> bool {
        buffers.update(std::iter::empty(), clients, false)
    }

    fn close(buffers: &mut SidebarBuffers, name: &str) {
        buffers.close(&history::Kind::Channel(server(), channel(name)));
    }

    fn is_saved(buffers: &SidebarBuffers, name: &str) -> bool {
        buffers
            .to_vec()
            .contains(&(server(), Target::Channel(channel(name))))
    }

    // Counts how often the channel is listed, so the sidebar shows it once.
    fn times_disconnected(buffers: &SidebarBuffers, name: &str) -> usize {
        buffers
            .disconnected_channels(&server())
            .filter(|disconnected| **disconnected == channel(name))
            .count()
    }

    #[test]
    fn kicked_channel_stays_saved_and_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);

        kick(&mut clients, &mut buffers, "#halloy");
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
        assert!(!update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
    }

    #[test]
    fn rejoined_channel_is_no_longer_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);

        // Parting again keeps the buffer open again.
        part(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
    }

    #[test]
    fn left_channel_is_listed_once_after_reconnect() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        update(&mut buffers, &clients);
        connect(&mut clients);
        update(&mut buffers, &clients);

        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
    }

    #[test]
    fn joined_channels_stay_disconnected_with_server() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        join(&mut clients, &mut buffers, "#rust");
        update(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        assert!(!update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_saved(&buffers, "#rust"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
        assert_eq!(times_disconnected(&buffers, "#rust"), 1);

        // Only channels joined again after reconnecting are connected.
        connect(&mut clients);
        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_saved(&buffers, "#rust"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
        assert_eq!(times_disconnected(&buffers, "#rust"), 1);
    }

    #[test]
    fn closing_channel_is_not_disconnected_with_server() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        // The PART is in flight, so the channel is still joined.
        close(&mut buffers, "#halloy");
        disconnect(&mut clients, &mut buffers);
        update(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
    }

    #[test]
    fn joined_channels_stay_disconnected_when_restore_on_launch_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        update_disabled(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
    }

    #[test]
    fn part_for_untracked_channel_is_not_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        part(&mut clients, &mut buffers, "#halloy");
        kick(&mut clients, &mut buffers, "#rust");
        update(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
        assert_eq!(times_disconnected(&buffers, "#rust"), 0);
    }

    #[test]
    fn closed_channel_is_removed_when_part_arrives_before_update() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&mut buffers, "#halloy");
        part(&mut clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
    }

    #[test]
    fn closed_channel_is_removed_when_part_arrives_after_update() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&mut buffers, "#halloy");

        // The PART is in flight, so the channel is still joined.
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));

        part(&mut clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
    }

    #[test]
    fn closed_disconnected_channel_is_removed() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);

        // A later join is not mistaken for the closed channel's PART.
        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
    }

    #[test]
    fn left_channels_stay_disconnected_when_restore_on_launch_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        join(&mut clients, &mut buffers, "#rust");
        assert!(!update_disabled(&mut buffers, &clients));

        kick(&mut clients, &mut buffers, "#halloy");
        part(&mut clients, &mut buffers, "#rust");
        assert!(!update_disabled(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_saved(&buffers, "#rust"));
        assert_eq!(times_disconnected(&buffers, "#halloy"), 1);
        assert_eq!(times_disconnected(&buffers, "#rust"), 1);

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);

        close(&mut buffers, "#rust");
        update_disabled(&mut buffers, &clients);
        assert_eq!(times_disconnected(&buffers, "#rust"), 0);
    }

    #[test]
    fn closed_channel_is_not_kept_when_restore_on_launch_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);

        close(&mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        part(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        assert_eq!(times_disconnected(&buffers, "#halloy"), 0);
    }
}
