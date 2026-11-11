use std::sync::Arc;
use std::time::Duration;

use data::client::{self, ClientsContext};
use data::config::actions::NicknameClickAction;
use data::dashboard::BufferAction;
use data::history::filter::FilterChain;
use data::history::{self, search, storage};
use data::target::{self, Target};
use data::user::Nick;
use data::{Config, Server, buffer, message, preview};
use hashbrown::{HashMap, HashSet};
use iced::widget::{
    self, button, center, column, container, operation, row, rule, scrollable,
    text, text_input,
};
use iced::{ContentFit, Length, Task, padding};
use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use tokio::time;

use super::{context_menu, message_feed, scroll_view};
use crate::widget::Element;
use crate::{Theme, font, icon, theme};

const DEBOUNCE: Duration = Duration::from_millis(200);
const REMOTE_DEBOUNCE: Duration = Duration::from_millis(800);

#[derive(Debug, Clone)]
pub enum Message {
    Input(String),
    Debounced(u64),
    RemoteDebounced(u64),
    Results(u64, search::Range, Result<search::Page, search::Error>),
    RemoteResults(u64, Server, Result<search::ServerPage, search::Error>),
    LoadMore,
    Scrolled(bool),
    Feed(scroll_view::Message),
}

pub enum Event {
    ContextMenu(context_menu::Event),
    OpenBuffer(Server, Target, BufferAction),
    GoToMessage(buffer::Upstream, message::MessageLink, BufferAction),
    OpenUrl(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Stale,
    Loading,
    Loaded,
    Updating,
    Failed(String),
}

#[derive(Debug, Clone)]
struct Hit {
    buffer: buffer::Upstream,
    kind: history::Kind,
    date: Date,
    message: data::Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteStatus {
    Loading,
    More,
    Done,
}

#[derive(Debug, Clone)]
struct Remote {
    server: Server,
    status: RemoteStatus,
    hits: Vec<Arc<Hit>>,
    before: Option<Timestamp>,
}

impl Remote {
    fn frontier(&self) -> Option<Timestamp> {
        (self.status != RemoteStatus::Done)
            .then_some(self.before)
            .flatten()
    }
}

#[derive(Debug, Clone)]
pub struct Search {
    pub query: String,
    input_id: widget::Id,
    scrollable_id: widget::Id,
    generation: u64,
    status: Status,
    hits: Vec<Arc<Hit>>,
    local: Vec<Arc<Hit>>,
    remotes: Vec<Remote>,
    newest: Option<search::Cursor>,
    next: Option<search::Cursor>,
    behind: bool,
    scrolled: bool,
}

impl Search {
    pub fn new(query: Option<String>) -> Self {
        let mut search = Self {
            query: String::new(),
            input_id: widget::Id::unique(),
            scrollable_id: widget::Id::unique(),
            generation: 0,
            status: Status::Idle,
            hits: vec![],
            local: vec![],
            remotes: vec![],
            newest: None,
            next: None,
            behind: false,
            scrolled: false,
        };
        search.set_query(query.unwrap_or_default());
        search
    }

    pub fn set_query(&mut self, query: String) {
        self.hits.clear();
        self.local.clear();
        self.change_query(query);
    }

    fn change_query(&mut self, query: String) {
        self.query = query;
        self.generation += 1;
        self.remotes.clear();
        self.newest = None;
        self.next = None;
        self.behind = false;
        self.status = if search::Query::parse(&self.query).is_runnable() {
            Status::Stale
        } else {
            self.hits.clear();
            self.local.clear();
            Status::Idle
        };
    }

    pub fn has_more(&self) -> bool {
        self.cutoff().is_some()
    }

    fn local_frontier(&self) -> Option<Timestamp> {
        self.next
            .and(self.local.last())
            .map(|hit| hit.message.time.utc)
    }

    fn cutoff(&self) -> Option<Timestamp> {
        self.remotes
            .iter()
            .filter_map(Remote::frontier)
            .chain(self.local_frontier())
            .max()
    }

    fn merge(&mut self) {
        let mut hits = self.local.clone();

        if self.remotes.iter().any(|remote| !remote.hits.is_empty()) {
            let msgids = self
                .local
                .iter()
                .filter_map(|hit| {
                    hit.message
                        .id
                        .as_ref()
                        .map(|id| (hit.buffer.as_server(), id))
                })
                .collect::<HashSet<_>>();

            // text is only compared between hits in the same buffer and time.
            let mut local = HashMap::<_, Vec<&Hit>>::new();
            for hit in &self.local {
                local
                    .entry((&hit.buffer, hit.message.time.utc))
                    .or_default()
                    .push(hit);
            }

            hits.extend(
                self.remotes
                    .iter()
                    .flat_map(|remote| &remote.hits)
                    .filter(|hit| {
                        !hit.message.id.as_ref().is_some_and(|id| {
                            msgids.contains(&(hit.buffer.as_server(), id))
                        }) && !local
                            .get(&(&hit.buffer, hit.message.time.utc))
                            .is_some_and(|stored| {
                                stored.iter().any(|stored| {
                                    stored.message.text() == hit.message.text()
                                })
                            })
                    })
                    .cloned(),
            );
        }

        let cutoff = self.cutoff();

        hits.retain(|hit| {
            cutoff.is_none_or(|cutoff| hit.message.time.utc >= cutoff)
        });
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.message.time.utc));

        self.hits = hits;
    }

    fn is_remote_loading(&self) -> bool {
        self.remotes
            .iter()
            .any(|remote| remote.status == RemoteStatus::Loading)
    }

    pub fn result_count(&self) -> Option<usize> {
        self.is_loaded().then_some(self.hits.len())
    }

    fn is_loaded(&self) -> bool {
        matches!(self.status, Status::Loaded | Status::Updating)
    }

    pub fn indexed(&mut self, storage: &storage::Manager) -> Task<Message> {
        self.behind = true;
        self.catch_up(storage)
    }

    pub fn update(
        &mut self,
        message: Message,
        clients: &mut client::Map,
        storage: &storage::Manager,
        config: &Config,
    ) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Input(query) => {
                self.change_query(query);

                if self.status == Status::Stale {
                    let generation = self.generation;

                    (
                        Task::perform(time::sleep(DEBOUNCE), move |()| {
                            Message::Debounced(generation)
                        }),
                        None,
                    )
                } else {
                    (Task::none(), None)
                }
            }
            Message::Debounced(generation) => {
                if generation == self.generation && self.status == Status::Stale
                {
                    let remote = Task::perform(
                        time::sleep(REMOTE_DEBOUNCE.saturating_sub(DEBOUNCE)),
                        move |()| Message::RemoteDebounced(generation),
                    );

                    (
                        Task::batch([
                            self.run(storage, search::Range::Latest),
                            remote,
                        ]),
                        None,
                    )
                } else {
                    (Task::none(), None)
                }
            }
            Message::RemoteDebounced(generation) => {
                if generation == self.generation && self.remotes.is_empty() {
                    (self.run_remote(clients), None)
                } else {
                    (Task::none(), None)
                }
            }
            Message::LoadMore => {
                let Some(cutoff) = self.cutoff() else {
                    return (Task::none(), None);
                };

                let mut tasks = vec![];

                if self.status == Status::Loaded
                    && let Some(next) = self.next
                    && self.local_frontier() == Some(cutoff)
                {
                    tasks.push(self.run(storage, search::Range::Before(next)));
                }

                let query = search::Query::parse(&self.query);
                let generation = self.generation;

                for remote in &mut self.remotes {
                    if remote.status != RemoteStatus::More
                        || remote.before != Some(cutoff)
                    {
                        continue;
                    }

                    if let Some(task) = remote_search(
                        clients,
                        &remote.server,
                        &query,
                        remote.before,
                        generation,
                    ) {
                        remote.status = RemoteStatus::Loading;
                        tasks.push(task);
                    } else {
                        remote.status = RemoteStatus::Done;
                    }
                }

                self.merge();

                (Task::batch(tasks), None)
            }
            Message::RemoteResults(generation, server, result) => {
                if generation != self.generation {
                    return (Task::none(), None);
                }

                let Some(remote) = self
                    .remotes
                    .iter_mut()
                    .find(|remote| remote.server == server)
                else {
                    return (Task::none(), None);
                };

                match result {
                    Ok(page) => {
                        remote.status = if page.more && page.oldest.is_some() {
                            RemoteStatus::More
                        } else {
                            RemoteStatus::Done
                        };
                        remote.before = page.oldest.or(remote.before);

                        remote.hits.extend(page.messages.into_iter().map(
                            |message| Arc::new(remote_hit(&server, message)),
                        ));
                    }
                    Err(error) => {
                        log::warn!("[{server}] server search failed: {error}");
                        remote.status = RemoteStatus::Done;
                    }
                }

                self.merge();

                (Task::none(), None)
            }
            Message::Scrolled(scrolled) => {
                self.scrolled = scrolled;
                (self.catch_up(storage), None)
            }
            Message::Results(generation, range, result) => {
                if generation != self.generation {
                    return (Task::none(), None);
                }

                let page = match result {
                    Ok(page) => page,
                    Err(error) => {
                        log::warn!("search failed: {error}");
                        if range == search::Range::Latest {
                            self.local.clear();
                            self.next = None;
                            self.merge();
                        }
                        self.status = Status::Failed(error.to_string());
                        return (Task::none(), None);
                    }
                };

                let servers = clients
                    .servers()
                    .map(|server| (format!("{server:b}"), server))
                    .collect::<Vec<_>>();
                let hits = page
                    .hits
                    .into_iter()
                    .filter_map(|hit| resolve(hit, &servers, clients))
                    .map(Arc::new);

                let task = match range {
                    search::Range::Before(_) => {
                        self.local.extend(hits);
                        self.next = page.next;
                        Task::none()
                    }
                    // Fewer than a page newer, so no gap to the current hits.
                    search::Range::After(_) if page.next.is_none() => {
                        self.local.splice(0..0, hits);
                        self.newest = page.newest.or(self.newest);
                        Task::none()
                    }
                    search::Range::Latest | search::Range::After(_) => {
                        self.local.clear();
                        self.local.extend(hits);
                        self.newest = page.newest;
                        self.next = page.next;
                        self.scrolled = false;
                        operation::snap_to(
                            self.scrollable_id.clone(),
                            scrollable::RelativeOffset::START,
                            operation::Animation::Instant,
                        )
                    }
                };
                self.status = Status::Loaded;
                self.merge();

                (Task::batch([task, self.catch_up(storage)]), None)
            }
            Message::Feed(message) => {
                (Task::none(), feed_event(message, config))
            }
        }
    }

    fn catch_up(&mut self, storage: &storage::Manager) -> Task<Message> {
        if self.behind && !self.scrolled && self.status == Status::Loaded {
            self.behind = false;
            let range = self
                .newest
                .map_or(search::Range::Latest, search::Range::After);
            self.run(storage, range)
        } else {
            Task::none()
        }
    }

    fn run(
        &mut self,
        storage: &storage::Manager,
        range: search::Range,
    ) -> Task<Message> {
        self.status = if self.status == Status::Loaded {
            Status::Updating
        } else {
            Status::Loading
        };

        let generation = self.generation;

        Task::perform(
            storage.search(search::Query::parse(&self.query), range),
            move |result| Message::Results(generation, range, result),
        )
    }

    fn run_remote(&mut self, clients: &mut client::Map) -> Task<Message> {
        let query = search::Query::parse(&self.query);
        let servers = clients.servers().cloned().collect::<Vec<_>>();
        let mut tasks = vec![];

        for server in servers {
            if let Some(task) =
                remote_search(clients, &server, &query, None, self.generation)
            {
                self.remotes.push(Remote {
                    server,
                    status: RemoteStatus::Loading,
                    hits: vec![],
                    before: None,
                });
                tasks.push(task);
            }
        }

        Task::batch(tasks)
    }

    pub fn focus(&self) -> Task<Message> {
        let input_id = self.input_id.clone();

        let focus =
            operation::is_focused(input_id.clone()).then(move |is_focused| {
                if is_focused {
                    Task::none()
                } else {
                    operation::focus(input_id.clone())
                }
            });

        if self.status == Status::Stale {
            focus.chain(Task::done(Message::Debounced(self.generation)))
        } else {
            focus
        }
    }
}

fn remote_search(
    clients: &mut client::Map,
    server: &Server,
    query: &search::Query,
    before: Option<Timestamp>,
    generation: u64,
) -> Option<Task<Message>> {
    let search = clients
        .client_mut(server)?
        .request_soju_search(query, before)?;
    let server = server.clone();

    Some(Task::perform(search, move |result| {
        Message::RemoteResults(generation, server, result)
    }))
}

fn remote_hit(server: &Server, message: data::Message) -> Hit {
    let buffer = match &message.target {
        message::Target::Channel { channel } => {
            buffer::Upstream::Channel(server.clone(), channel.clone())
        }
        message::Target::Query { query } => {
            buffer::Upstream::Query(server.clone(), query.clone())
        }
        _ => buffer::Upstream::Server(server.clone()),
    };

    Hit {
        kind: history::Kind::from(buffer.clone()),
        date: message.time.system_date(),
        buffer,
        message,
    }
}

fn resolve(
    hit: search::Hit,
    servers: &[(String, &Server)],
    clients: &client::Map,
) -> Option<Hit> {
    let server = servers
        .iter()
        .find_map(|(key, server)| (*key == hit.server).then_some(*server))?
        .clone();

    let buffer = match hit.buffer {
        search::Buffer::Server => buffer::Upstream::Server(server),
        search::Buffer::Channel(channel) => {
            let channel = target::Channel::from_str(
                &channel,
                clients.get_server_chantypes_or_default(&server),
                clients.get_server_casemapping_or_default(&server),
            );
            buffer::Upstream::Channel(server, channel)
        }
        search::Buffer::Query(query) => {
            let query = target::Query::from(Nick::from_str(
                &query,
                clients.get_server_casemapping_or_default(&server),
            ));
            buffer::Upstream::Query(server, query)
        }
    };

    Some(Hit {
        kind: history::Kind::from(buffer.clone()),
        date: hit.message.time.system_date(),
        buffer,
        message: hit.message,
    })
}

fn feed_event(message: scroll_view::Message, config: &Config) -> Option<Event> {
    match message {
        scroll_view::Message::ContextMenu(message) => {
            context_menu::update(message).map(Event::ContextMenu)
        }
        scroll_view::Message::Link(link) => match link {
            message::Link::GoToMessage(buffer, message, buffer_action) => {
                Some(Event::GoToMessage(
                    buffer,
                    message,
                    buffer_action.unwrap_or_default(),
                ))
            }
            message::Link::Channel(server, channel, buffer_action) => {
                buffer_action.map(|buffer_action| {
                    Event::OpenBuffer(
                        server,
                        Target::Channel(channel),
                        buffer_action,
                    )
                })
            }
            message::Link::Url(url) => Some(Event::OpenUrl(url)),
            message::Link::User(server, user) => {
                match config.actions.buffer.click_username {
                    NicknameClickAction::OpenQuery(buffer_action) => {
                        Some(Event::OpenBuffer(
                            server,
                            Target::Query(target::Query::from(user)),
                            buffer_action,
                        ))
                    }
                    NicknameClickAction::InsertNickname
                    | NicknameClickAction::Noop => None,
                }
            }
            message::Link::ExpandMessage(..)
            | message::Link::ContractMessage(..) => None,
        },
        _ => None,
    }
}

pub fn view<'a>(
    state: &'a Search,
    clients: &'a client::Map,
    previews: &'a preview::Collection,
    filter_chain: FilterChain<'_>,
    config: &'a Config,
    theme: &'a Theme,
    channels_context: &'a dyn context_menu::ChannelsContext,
) -> Element<'a, Message> {
    let header = container(
        column![
            row![
                text_input("Search messages...", &state.query)
                    .id(state.input_id.clone())
                    .style(move |theme, status| {
                        if matches!(status, text_input::Status::Disabled) {
                            theme::text_input::primary(
                                theme,
                                text_input::Status::Active,
                            )
                        } else {
                            theme::text_input::primary(theme, status)
                        }
                    })
                    .on_input(Message::Input),
            ]
            .spacing(8)
            .padding(padding::top(8)),
            container(rule::horizontal(1)).width(Length::Fill)
        ]
        .spacing(8),
    )
    .padding(padding::horizontal(4))
    .width(Length::Fill);

    let show_dates = config.buffer.date_separators.show;
    let append_year = config
        .buffer
        .date_separators
        .format
        .as_ref()
        .is_some_and(|format| !has_year(format));
    let this_year = Zoned::now().year();
    let mut last_date = None;
    let mut rows = vec![];

    for hit in state.hits.iter().filter(|hit| {
        !filter_chain.filter_message_of_kind(&hit.message, &hit.kind)
    }) {
        let Some(row) = message_feed::message_row(
            &hit.message,
            hit.buffer.clone(),
            hit.buffer.as_server(),
            hit.buffer.as_channel(),
            if hit.message.history_id == history::Id::Undetermined {
                message::MessageLink::Remote(message::Pointer {
                    id: hit.message.id.clone(),
                    time: hit.message.time,
                })
            } else {
                message::MessageLink::Message(
                    hit.message.history_id,
                    hit.message.time,
                )
            },
            clients,
            previews,
            config,
            theme,
            channels_context,
        ) else {
            continue;
        };

        if show_dates && last_date != Some(hit.date) {
            last_date = Some(hit.date);

            let show_year = hit.date.year() != this_year;
            let mut label =
                config.buffer.format_date_separator(&hit.date, show_year);
            if show_year && append_year {
                label = format!("{label}, {}", hit.date.year());
            }

            rows.push(scroll_view::date_separator(label, config, theme));
        }

        rows.push(row.map(Message::Feed));
    }

    let body: Element<'a, Message> = if rows.is_empty() {
        let reason = match &state.status {
            Status::Idle => None,
            Status::Stale | Status::Loading => Some("...".to_string()),
            Status::Loaded | Status::Updating if state.is_remote_loading() => {
                Some("...".to_string())
            }
            Status::Loaded | Status::Updating => {
                Some("No messages found".to_string())
            }
            Status::Failed(error) => Some(error.clone()),
        };

        placeholder(reason, theme)
    } else {
        let load_more = (state.has_more() && state.is_loaded()).then(|| {
            container(
                button(text("Load more"))
                    .style(|theme, status| {
                        theme::button::secondary(theme, status, false)
                    })
                    .on_press(Message::LoadMore),
            )
            .center_x(Length::Fill)
            .padding(8)
        });

        scrollable(
            column(rows)
                .push(load_more)
                .spacing(config.buffer.line_spacing)
                .padding(8),
        )
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::default()
                .width(config.pane.scrollbar.width)
                .scroller_width(config.pane.scrollbar.scroller_width),
        ))
        .on_scroll(|scroll| {
            Message::Scrolled(scroll.viewport.absolute_offset().y > 0.0)
        })
        .id(state.scrollable_id.clone())
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    };

    let content = column![header, body].spacing(1).padding([2, 2]);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn has_year(format: &str) -> bool {
    // Same month, day and weekday; only the year differs.
    let date = |year| {
        Date::new(year, 1, 1)
            .ok()
            .as_ref()
            .map(|date| Date::strftime(date, format).to_string())
    };
    date(2001) != date(2007)
}

fn placeholder<'a>(
    reason: Option<String>,
    theme: &'a Theme,
) -> Element<'a, Message> {
    let content = match reason {
        Some(reason) => column![
            text(reason)
                .style(theme::text::secondary)
                .font_maybe(theme::font_style::secondary(theme).map(font::get))
        ],
        None => column![
            icon::search()
                .width(Length::Shrink)
                .content_fit(ContentFit::Contain)
                .height(theme::TEXT_SIZE + 3.0)
                .style(theme::text::secondary),
            text("Search messages")
                .style(theme::text::secondary)
                .font_maybe(theme::font_style::secondary(theme).map(font::get)),
        ]
        .spacing(8)
        .align_x(iced::Alignment::Center),
    };

    center(content).into()
}
