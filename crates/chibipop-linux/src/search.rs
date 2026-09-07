use crate::paths::Paths;
use crate::popup::text::TextEngine;
use crate::search_popup::Definition;
use anyhow::{Context, Result};
use chibipop::config::Config;
use chibipop::search::{candidates, selected_presentation, SearchMode, SearchResult,
    SearchService, SentenceAnalyzer, SentenceToken};
use chibipop::ui::theme::Theme;
use chibipop_linux::media::MediaSurfaces;
use iced::widget::{button, column, container, image, mouse_area, rich_text, row,
    scrollable, span, text, text_editor, text_input};
use iced::{window, Color, Element, Length, Point, Size, Task};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;

const MAX_DEFINITIONS: usize = 8;

fn focus_path(paths: &Paths) -> Result<PathBuf> {
    let display = crate::wayland::display_name()?;
    Ok(paths.runtime_dir()?.join(format!("search-focus-{}.lock", crate::lock::sanitize(&display))))
}

pub fn is_focused(paths: &Paths) -> Result<bool> {
    focus::is_held(&focus_path(paths)?).context("probing search focus")
}

fn search_command_base(paths: &Paths, mode: SearchMode) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--config").arg(&paths.config_file).arg(match mode {
        SearchMode::Dictionary => "search", SearchMode::Sentence => "sentence-search",
    }).arg("--data-dir").arg(&paths.data_dir);
    crate::signals::unmasked(&mut command);
    Ok(command)
}

pub fn search_command_mode(paths: &Paths, mode: SearchMode, initial: Option<&str>) -> std::io::Result<Command> {
    let mut command = search_command_base(paths, mode)?;
    if let Some(initial) = initial { command.arg("--text").arg(initial); }
    Ok(command)
}

pub(crate) fn search_stdin_command_mode(paths: &Paths, mode: SearchMode) -> std::io::Result<Command> {
    let mut command = search_command_base(paths, mode)?;
    command.arg("--read-stdin").stdin(Stdio::piped());
    Ok(command)
}

pub fn run(paths: Paths, mode: SearchMode, initial: String) -> Result<()> {
    let _instance = if initial.is_empty() {
        let display = crate::wayland::display_name()?;
        let mode_name = match mode { SearchMode::Dictionary => "dictionary", SearchMode::Sentence => "sentence" };
        let name = format!("search-{mode_name}-{}.lock", crate::lock::sanitize(&display));
        match crate::lock::acquire_at(paths.runtime_dir()?, &name) {
            Ok(lock) => Some(lock),
            Err(crate::lock::LockError::AlreadyRunning { .. }) => return Ok(()),
            Err(crate::lock::LockError::Io(error)) => return Err(error.into()),
        }
    } else { None };
    let config = chibipop::config::load_or_create(&paths.config_file)?;
    let focus_path = focus_path(&paths)?;
    focus::prepare(&focus_path).context("preparing search focus")?;
    let worker = std::sync::Arc::new(Worker::new(paths)?);
    let mut engine = TextEngine::new(&config.popup.font);
    let theme = crate::search_popup::theme(&config, &mut engine);
    let font = iced::Font::with_name(Box::leak(theme.font_name.into_boxed_str()));
    iced::daemon(move || {
        let (main, open) = window::open(window::Settings {
            size: Size::new(760.0, 640.0), min_size: Some(Size::new(340.0, 260.0)),
            ..window::Settings::default()
        });
        let mut engine = TextEngine::new(&config.popup.font);
        let theme = crate::search_popup::theme(&config, &mut engine);
        let mut search = Search {
            mode, query: initial.clone(), editor: text_editor::Content::with_text(&initial),
            tokens: Vec::new(), selected: None, result: SearchResult::Empty, status: String::new(),
            generation: 0, worker: worker.clone(), busy: false, pending: None,
            main, definitions: Vec::new(), config: config.clone(), theme, font, engine,
            media: None, focused: HashSet::new(), hovered: HashSet::new(),
            focus_path: focus_path.clone(), focus: None,
        };
        let request = search.input_changed();
        (search, Task::batch([open.map(Message::Opened), request]))
    }, update, view)
        .title(|search: &Search, id| if id == search.main { title(search.mode) } else { "chibipop definition" }.to_string())
        .default_font(font)
        .theme(|search: &Search, _| iced::Theme::custom("Popup", iced::theme::Palette {
            background: color(search.theme.background), text: color(search.theme.body_text),
            primary: color(search.theme.accent), success: color(search.theme.accent),
            warning: color(search.theme.dict_label_text), danger: color(search.theme.accent),
        }))
        .style(|_, _| iced::theme::Style { background_color: Color::TRANSPARENT,
            text_color: Color::WHITE })
        .subscription(|_| iced::event::listen_with(|event, _, id| match event {
            iced::Event::Window(event) => Some(Message::Window(id, event)),
            iced::Event::Mouse(iced::mouse::Event::CursorEntered) => Some(Message::Presence(id, true)),
            iced::Event::Mouse(iced::mouse::Event::CursorLeft) => Some(Message::Presence(id, false)),
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), ..
            }) => Some(Message::Back(id)),
            _ => None,
        }))
        .run().context("running search")
}

fn title(mode: SearchMode) -> &'static str {
    match mode { SearchMode::Dictionary => "Dictionary search", SearchMode::Sentence => "Sentence search" }
}

struct Search {
    mode: SearchMode,
    query: String,
    editor: text_editor::Content,
    tokens: Vec<SentenceToken>,
    selected: Option<usize>,
    result: SearchResult,
    status: String,
    generation: u64,
    worker: std::sync::Arc<Worker>,
    busy: bool,
    pending: Option<Job>,
    main: window::Id,
    definitions: Vec<(window::Id, Definition)>,
    config: Config,
    theme: Theme,
    font: iced::Font,
    engine: TextEngine,
    media: Option<MediaSurfaces>,
    focus_path: PathBuf,
    focus: Option<focus::Lease>,
    focused: HashSet<window::Id>,
    hovered: HashSet<window::Id>,
}

#[derive(Clone, Debug)]
enum Target { Input(u64), Word(u64), Hover(window::Id, u64) }

#[derive(Clone, Debug)]
struct Job { target: Target, query: String, tokenize: bool }

#[derive(Clone, Debug)]
struct Reply { result: SearchResult, tokens: Vec<SentenceToken>, config: Config, database: PathBuf }

#[derive(Clone, Debug)]
enum Message {
    Input(String), Edit(text_editor::Action), Submit, Word(usize), Candidate(usize),
    Finished(Target, std::result::Result<Box<Reply>, String>), Opened(window::Id),
    Window(window::Id, window::Event), Presence(window::Id, bool),
    Hover(window::Id, Point), Leave(window::Id), Scroll(window::Id, iced::mouse::ScrollDelta),
    Scale(window::Id, f32), Back(window::Id), Click(window::Id),
}

type WorkerReply = iced::futures::channel::oneshot::Sender<std::result::Result<Box<Reply>, String>>;
struct Worker { sender: mpsc::Sender<(Job, WorkerReply)> }

impl Worker {
    fn new(paths: Paths) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<(Job, WorkerReply)>();
        std::thread::Builder::new().name("search-worker".into()).spawn(move || {
            let mut analyzer = SentenceAnalyzer::new(data_file("data/ipadic/system.dic"));
            for (job, reply) in receiver {
                let mut run = || -> Result<Reply> {
                    let config = chibipop::config::load_or_create(&paths.config_file)?;
                    let database = paths.data_dir.join("chibipop.sqlite");
                    let tokens = if job.tokenize { analyzer.tokenize(&job.query) } else { Vec::new() };
                    let result = if job.tokenize || job.query.trim().is_empty() { SearchResult::Empty }
                        else { SearchService::open(&database, &data_file("data/deconjugator.json"), &config)?.search(&job.query)? };
                    Ok(Reply { result, tokens, config, database })
                };
                let _ = reply.send(run().map(Box::new).map_err(|error| format!("Search failed: {error:#}")));
            }
        })?;
        Ok(Self { sender })
    }
}

impl Search {
    fn enqueue(&mut self, job: Job) -> Task<Message> {
        if self.busy {
            let input_pending = self.pending.as_ref().is_some_and(|pending|
                matches!(pending.target, Target::Input(_) | Target::Word(_)));
            if !input_pending || !matches!(job.target, Target::Hover(_, _)) { self.pending = Some(job); }
            return Task::none();
        }
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        let target = job.target.clone();
        if self.worker.sender.send((job, sender)).is_err() {
            self.status = "Search worker stopped. Close this window and reopen search.".into();
            return Task::none();
        }
        self.busy = true;
        Task::perform(receiver, move |reply| Message::Finished(target.clone(),
            reply.unwrap_or_else(|_| Err("Search worker stopped unexpectedly.".into()))))
    }

    fn input_changed(&mut self) -> Task<Message> {
        self.generation = self.generation.wrapping_add(1);
        self.tokens.clear();
        self.selected = None;
        self.result = SearchResult::Empty;
        self.status = if self.query.trim().is_empty() { "Type or paste Japanese text." } else { "Searching…" }.into();
        let close = self.close_from(0);
        let query = self.enqueue(Job { target: Target::Input(self.generation), query: self.query.clone(),
            tokenize: self.mode == SearchMode::Sentence });
        Task::batch([close, query])
    }

    fn protect(&mut self) {
        if self.focused.is_empty() && self.hovered.is_empty() { self.focus = None; }
        else if self.focus.is_none() {
            match focus::Lease::acquire(&self.focus_path) {
                Ok(lease) => self.focus = Some(lease),
                Err(error) => self.status = format!("Cannot protect search focus: {error}"),
            }
        }
    }

    fn close_from(&mut self, depth: usize) -> Task<Message> {
        let mut tasks = Vec::new();
        for (id, _) in self.definitions.drain(depth..) {
            self.focused.remove(&id);
            self.hovered.remove(&id);
            tasks.push(window::close(id));
        }
        self.protect();
        Task::batch(tasks)
    }

    fn open_definition(&mut self, presentation: chibipop::present::Presentation) -> Task<Message> {
        if self.definitions.len() >= MAX_DEFINITIONS {
            self.status = "Close a definition window before opening another level.".into();
            return Task::none();
        }
        match Definition::new(presentation, &self.config, &self.theme, &mut self.engine, self.media.as_mut()) {
            Ok(definition) => {
                let (id, open) = window::open(window::Settings {
                    size: definition.size, min_size: Some(definition.size), max_size: Some(definition.size),
                    resizable: false,
                    decorations: false, transparent: true, minimizable: false,
                    level: window::Level::AlwaysOnTop,
                    platform_specific: window::settings::PlatformSpecific {
                        application_id: "chibipop.search.definition".into(), override_redirect: true,
                    },
                    ..window::Settings::default()
                });
                self.definitions.push((id, definition));
                open.map(Message::Opened)
            }
            Err(error) => { self.status = format!("Cannot render definition: {error:#}"); Task::none() }
        }
    }
}

fn update(search: &mut Search, message: Message) -> Task<Message> {
    match message {
        Message::Input(query) => { search.query = query; return search.input_changed(); }
        Message::Edit(action) => {
            let edit = action.is_edit();
            search.editor.perform(action);
            if edit { search.query = search.editor.text(); return search.input_changed(); }
        }
        Message::Submit => return search.input_changed(),
        Message::Word(index) => {
            let Some(token) = search.tokens.get(index).filter(|token| token.selectable) else { return Task::none() };
            let query = token.text.clone();
            search.selected = Some(index);
            search.generation = search.generation.wrapping_add(1);
            search.result = SearchResult::Empty;
            search.status = "Searching…".into();
            let close = search.close_from(0);
            let lookup = search.enqueue(Job {
                target: Target::Word(search.generation), query, tokenize: false,
            });
            return Task::batch([close, lookup]);
        }
        Message::Candidate(index) => {
            if let Some(presentation) = selected_presentation(&search.result, index) {
                let close = search.close_from(0);
                return Task::batch([close, search.open_definition(presentation)]);
            }
        }
        Message::Finished(target, reply) => {
            search.busy = false;
            let valid = match target {
                Target::Input(generation) | Target::Word(generation) => generation == search.generation,
                Target::Hover(id, generation) => search.definitions.iter().any(|(known, definition)|
                    *known == id && definition.generation == generation && definition.hovered.is_some()),
            };
            let mut tasks = Vec::new();
            if valid {
                match reply {
                    Err(error) => search.status = error,
                    Ok(reply) => match target {
                        Target::Input(_) | Target::Word(_) => {
                            search.config = reply.config;
                            let theme = crate::search_popup::theme(&search.config, &mut search.engine);
                            if theme.font_name != search.theme.font_name {
                                search.font = iced::Font::with_name(Box::leak(theme.font_name.clone().into_boxed_str()));
                            }
                            search.theme = theme;
                            search.media = MediaSurfaces::open(&reply.database).ok();
                            if matches!(target, Target::Input(_)) { search.tokens = reply.tokens; }
                            search.result = reply.result;
                            search.status = match search.result {
                                SearchResult::Found(_) => "Choose a candidate to open its definition.",
                                SearchResult::Miss => "No matching entries in the enabled dictionaries.",
                                SearchResult::Empty if search.mode == SearchMode::Sentence => "Paste a sentence, then click a word to see its candidates.",
                                SearchResult::Empty => "Type or paste a Japanese word or expression.",
                            }.into();
                        }
                        Target::Hover(id, _) => {
                            if let Some(presentation) = selected_presentation(&reply.result, 0) {
                                if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                                    tasks.push(search.close_from(depth + 1));
                                    tasks.push(search.open_definition(presentation));
                                }
                            }
                        }
                    },
                }
            }
            if let Some(job) = search.pending.take() { tasks.push(search.enqueue(job)); }
            return Task::batch(tasks);
        }
        Message::Opened(id) => {
            return if id == search.main { iced::widget::operation::focus("search-query") }
                else { window::scale_factor(id).map(move |scale| Message::Scale(id, scale)) };
        }
        Message::Presence(id, present) => {
            let known = id == search.main || search.definitions.iter().any(|(known, _)| *known == id);
            if present && known { search.hovered.insert(id); } else { search.hovered.remove(&id); }
            search.protect();
        }
        Message::Window(id, event) => match event {
            window::Event::Focused => {
                if id == search.main || search.definitions.iter().any(|(known, _)| *known == id) {
                    search.focused.insert(id);
                }
                search.protect();
            }
            window::Event::Unfocused => { search.focused.remove(&id); search.protect(); }
            window::Event::Closed => {
                if id == search.main { return iced::exit(); }
                if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                    return search.close_from(depth);
                }
            }
            window::Event::Resized(size) => {
                if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                    if let Err(error) = definition.resize(&search.config, &search.theme, &mut search.engine,
                        search.media.as_mut(), size, definition.scale) { search.status = error.to_string(); }
                }
            }
            window::Event::Rescaled(scale) => return update(search, Message::Scale(id, scale)),
            _ => {}
        },
        Message::Scale(id, scale) => {
            if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                if let Err(error) = definition.resize(&search.config, &search.theme, &mut search.engine,
                    search.media.as_mut(), definition.size, scale) { search.status = error.to_string(); }
            }
        }
        Message::Hover(id, point) => {
            let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) else { return Task::none() };
            let definition = &mut search.definitions[depth].1;
            definition.pointer = point;
            if !search.config.popup.sub_popups { return Task::none(); }
            let query = definition.hover(point, &search.theme, &mut search.engine);
            if query == definition.hovered { return Task::none(); }
            definition.hovered = query.clone();
            definition.generation = definition.generation.wrapping_add(1);
            let generation = definition.generation;
            if let Some(query) = query {
                let close = search.close_from(depth + 1);
                let lookup = search.enqueue(Job { target: Target::Hover(id, generation), query, tokenize: false });
                return Task::batch([close, lookup]);
            }
        }
        Message::Leave(id) => {
            if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                definition.hovered = None;
                definition.generation = definition.generation.wrapping_add(1);
            }
        }
        Message::Back(id) => {
            if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                let parent = depth.checked_sub(1).map_or(search.main, |index| search.definitions[index].0);
                return Task::batch([search.close_from(depth), window::gain_focus(parent)]);
            }
        }
        Message::Click(id) => {
            if let Some((_, definition)) = search.definitions.iter().find(|(known, _)| *known == id) {
                match definition.click() {
                    Some(chibipop::controller::HitAction::Back) => return update(search, Message::Back(id)),
                    Some(chibipop::controller::HitAction::DrillDown(query)) => {
                        let definition = &mut search.definitions.iter_mut().find(|(known, _)| *known == id).expect("existing definition").1;
                        definition.hovered = Some(query.clone());
                        definition.generation = definition.generation.wrapping_add(1);
                        let target = Target::Hover(id, definition.generation);
                        return search.enqueue(Job { target, query, tokenize: false });
                    }
                    _ => {}
                }
            }
        }
        Message::Scroll(id, delta) => {
            if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                let delta = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y * search.theme.body_size * 3.0,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                definition.scroll -= delta * definition.scale;
                definition.hovered = None;
                definition.generation = definition.generation.wrapping_add(1);
                if let Err(error) = definition.paint(&search.theme, &mut search.engine, search.media.as_mut()) {
                    search.status = error.to_string();
                }
            }
        }
    }
    Task::none()
}

fn color(rgb: (u8, u8, u8)) -> Color { Color::from_rgb8(rgb.0, rgb.1, rgb.2) }

fn border(theme: &Theme) -> iced::Border {
    iced::Border { color: color(theme.border), width: theme.border_width,
        radius: (theme.corner_radius as f32).into() }
}

fn view(search: &Search, id: window::Id) -> Element<'_, Message> {
    if id != search.main {
        let Some((_, definition)) = search.definitions.iter().find(|(known, _)| *known == id) else {
            return container(text("")).into();
        };
        return mouse_area(image(definition.image.clone())
            .width(definition.size.width).height(definition.size.height)
            .content_fit(iced::ContentFit::Fill))
            .on_move(move |point| Message::Hover(id, point))
            .on_press(Message::Click(id)).on_exit(Message::Leave(id))
            .on_scroll(move |delta| Message::Scroll(id, delta)).into();
    }
    let theme = &search.theme;
    let pad = theme.padding.max(0) as f32;
    let input: Element<'_, Message> = match search.mode {
        SearchMode::Dictionary => text_input("Japanese word or expression", &search.query)
            .id("search-query").on_input(Message::Input).on_submit(Message::Submit)
            .font(search.font).size(theme.body_size).padding(pad).style(move |_, _| text_input::Style {
                background: color(theme.background).into(), border: border(theme),
                icon: color(theme.body_text), placeholder: color(theme.dimmed_text),
                value: color(theme.body_text), selection: color(theme.accent),
            }).into(),
        SearchMode::Sentence => text_editor(&search.editor).id("search-query").on_action(Message::Edit)
            .font(search.font).size(theme.body_size).padding(pad).height(140).style(move |_, _| text_editor::Style {
                background: color(theme.background).into(), border: border(theme),
                placeholder: color(theme.dimmed_text), value: color(theme.body_text), selection: color(theme.accent),
            }).into(),
    };
    let mut content = column![text(title(search.mode)).font(search.font).size(theme.headword_size)
        .color(color(theme.headword_text)), input].spacing(pad);
    if search.mode == SearchMode::Sentence {
        let spans = search.tokens.iter().enumerate().map(|(index, token)| {
            let mut token_span = span(token.text.as_str()).color(color(theme.body_text));
            if token.selectable { token_span = token_span.link(index); }
            if search.selected == Some(index) {
                let mut accent = color(theme.accent);
                accent.a = chibipop::ui::layout::HIGHLIGHT_ALPHA;
                token_span = token_span.background(accent);
            }
            token_span
        }).collect::<Vec<_>>();
        content = content.push(scrollable(rich_text(spans).font(search.font).size(theme.body_size)
            .on_link_click(Message::Word)).height(120));
    }
    content = content.push(text(&search.status).font(search.font).size(theme.dimmed_size).color(color(theme.dimmed_text)));
    let mut rows = column![].spacing(pad);
    for candidate in candidates(&search.result) {
        let heading = row![text(candidate.headword).font(search.font).size(theme.headword_size).color(color(theme.headword_text)),
            text(candidate.reading).font(search.font).size(theme.reading_size).color(color(theme.reading_text))].spacing(pad);
        rows = rows.push(button(column![heading, text(candidate.summary).font(search.font).size(theme.body_size)]
            .spacing(pad / 2.0)).width(Length::Fill).padding(pad)
            .style(move |_, status| button::Style {
                background: Some(color(if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                    theme.separator
                } else { theme.background }).into()),
                text_color: color(theme.body_text), border: border(theme), ..button::Style::default()
            }).on_press(Message::Candidate(candidate.index)));
    }
    container(content.push(scrollable(rows).height(Length::Fill))).padding(pad)
        .style(move |_| container::Style { background: Some(color(theme.background).into()),
            text_color: Some(color(theme.body_text)), ..container::Style::default() }).into()
}

fn data_file(relative: &str) -> PathBuf {
    let beside = chibipop::paths::beside_exe(relative);
    if beside.is_file() { return beside; }
    let shared = chibipop::paths::beside_exe(&format!("../share/chibipop/{relative}"));
    if shared.is_file() { return shared; }
    chibipop::paths::data_file(relative)
}
mod focus {
    use std::fs::{File, TryLockError};
    use std::io;
    use std::path::Path;

    pub fn prepare(path: &Path) -> io::Result<()> {
        File::options().read(true).write(true).create(true).truncate(false).open(path)?;
        Ok(())
    }

    pub struct Lease(File);

    impl Lease {
        /// Publishers share the lock. Only a brief, nonblocking observer can
        /// hold the exclusive lock, so acquisition cannot miss a racing probe.
        pub fn acquire(path: &Path) -> io::Result<Self> {
            let file = File::open(path)?;
            file.lock_shared()?;
            Ok(Self(file))
        }
    }

    impl Drop for Lease {
        /// Release inherited copies too, as the daemon's InstanceLock does.
        fn drop(&mut self) {
            let _ = self.0.unlock();
        }
    }

    pub fn is_held(path: &Path) -> io::Result<bool> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        match file.try_lock() {
            Ok(()) => { file.unlock()?; Ok(false) }
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};

        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let path = std::env::temp_dir().join(format!("chibipop-focus-{}-{}",
                    std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
                std::fs::create_dir(&path).unwrap();
                Self(path)
            }
            fn path(&self) -> PathBuf { self.0.join("focus.lock") }
        }
        impl Drop for Fixture {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }

        #[test]
        fn missing_and_stale_files_do_not_report_focus() {
            let fixture = Fixture::new();
            let path = fixture.path();
            assert!(!is_held(&path).unwrap());
            assert!(!path.exists());
            std::fs::write(&path, "stale pid 123").unwrap();
            assert!(!is_held(&path).unwrap());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "stale pid 123");
        }

        #[test]
        fn focus_loss_drop_and_reopen_release_the_live_lock() {
            let fixture = Fixture::new();
            let path = fixture.path();
            prepare(&path).unwrap();
            for _ in 0..3 {
                let lease = Lease::acquire(&path).unwrap();
                assert!(is_held(&path).unwrap());
                drop(lease);
                assert!(path.exists());
                assert!(!is_held(&path).unwrap());
            }
        }

        #[test]
        fn probes_cannot_displace_publishers() {
            let fixture = Fixture::new();
            let path = fixture.path();
            prepare(&path).unwrap();
            let first = Lease::acquire(&path).unwrap();
            let second = Lease::acquire(&path).unwrap();
            for _ in 0..20 { assert!(is_held(&path).unwrap()); }
            drop(first);
            assert!(is_held(&path).unwrap());
            drop(second);
            assert!(!is_held(&path).unwrap());
        }

        #[test]
        fn focus_acquisition_survives_a_racing_probe() {
            let fixture = Fixture::new();
            let path = fixture.path();
            prepare(&path).unwrap();
            let observer = File::open(&path).unwrap();
            observer.try_lock().unwrap();
            let (acquired, result) = std::sync::mpsc::channel();
            let publisher = std::thread::spawn(move || {
                let _lease = Lease::acquire(&path).unwrap();
                acquired.send(()).unwrap();
            });
            assert_eq!(result.recv_timeout(std::time::Duration::from_millis(20)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout));
            observer.unlock().unwrap();
            result.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            publisher.join().unwrap();
        }

        #[test]
        fn separate_display_paths_are_independent() {
            let fixture = Fixture::new();
            let first = fixture.path();
            let second = fixture.0.join("another-display.lock");
            prepare(&first).unwrap();
            prepare(&second).unwrap();
            let _lease = Lease::acquire(&first).unwrap();
            assert!(is_held(&first).unwrap());
            assert!(!is_held(&second).unwrap());
        }

        #[test]
        fn probe_errors_remain_errors() {
            let path = Path::new("invalid\0focus.lock");
            assert!(is_held(path).is_err());
            assert!(Lease::acquire(path).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Search, mpsc::Receiver<(Job, WorkerReply)>) {
        let (sender, receiver) = mpsc::channel();
        let config = Config::default();
        let mut engine = TextEngine::new("Noto Sans CJK JP");
        let theme = crate::search_popup::theme(&config, &mut engine);
        (Search {
            mode: SearchMode::Dictionary, query: String::new(), editor: text_editor::Content::new(),
            tokens: Vec::new(), selected: None, result: SearchResult::Empty, status: String::new(),
            generation: 0, worker: std::sync::Arc::new(Worker { sender }), busy: false, pending: None,
            main: window::Id::unique(), definitions: Vec::new(), config, theme, font: iced::Font::DEFAULT, engine, media: None,
            focus_path: PathBuf::new(), focus: None, focused: HashSet::new(), hovered: HashSet::new(),
        }, receiver)
    }

    #[test]
    fn child_commands_preserve_mode_text_config_and_dictionary_paths() {
        let mut paths = crate::paths::resolve(&crate::paths::Env::from_process(), Some("/tmp/search config.toml".into()));
        paths.data_dir = "/tmp/portable dictionary".into();
        let command = search_command_mode(&paths, SearchMode::Sentence, Some("猫。\n犬。" )).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--config", "/tmp/search config.toml", "sentence-search", "--data-dir",
            "/tmp/portable dictionary", "--text", "猫。\n犬。"]);
        let command = search_command_mode(&paths, SearchMode::Dictionary, None).unwrap();
        assert!(command.get_args().any(|arg| arg == "search"));
        assert!(!command.get_args().any(|arg| arg == "--text"));
    }

    #[test]
    fn captured_text_uses_stdin_and_never_enters_the_command_line() {
        let mut paths = crate::paths::resolve(
            &crate::paths::Env::from_process(), Some("/tmp/search config.toml".into()),
        );
        paths.data_dir = "/tmp/portable dictionary".into();
        let command = search_stdin_command_mode(&paths, SearchMode::Sentence).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--config", "/tmp/search config.toml", "sentence-search",
            "--data-dir", "/tmp/portable dictionary", "--read-stdin"]);
    }

    #[test]
    fn rapid_input_rejects_stale_results_and_runs_the_latest_query() {
        let (mut search, receiver) = fixture();
        let _ = update(&mut search, Message::Input("猫".into()));
        let (first, _) = receiver.recv().unwrap();
        let _ = update(&mut search, Message::Input("犬".into()));
        let _ = update(&mut search, Message::Input("食べました".into()));
        assert!(receiver.try_recv().is_err());
        let _ = update(&mut search, Message::Finished(first.target, Err("stale failure".into())));
        assert_eq!(receiver.recv().unwrap().0.query, "食べました");
        assert_ne!(search.status, "stale failure");
        assert_eq!(search.query, "食べました");
        assert!(search.busy);
    }

    #[test]
    fn hover_cannot_displace_pending_input_and_clear_discards_old_candidates() {
        let (mut search, receiver) = fixture();
        let _ = update(&mut search, Message::Input("猫".into()));
        let (first, _) = receiver.recv().unwrap();
        let _ = update(&mut search, Message::Input(String::new()));
        let _ = search.enqueue(Job { target: Target::Hover(window::Id::unique(), 0), query: "犬".into(), tokenize: false });
        let _ = update(&mut search, Message::Finished(first.target, Ok(Box::new(Reply {
            result: SearchResult::Found(Box::new(crate::popup::canned())), tokens: Vec::new(),
            config: Config::default(), database: PathBuf::new(),
        }))));
        assert_eq!(search.result, SearchResult::Empty);
        assert!(receiver.recv().unwrap().0.query.is_empty());
    }

    #[test]
    fn clicking_the_second_repeated_word_keeps_its_exact_sentence_range() {
        let (mut search, receiver) = fixture();
        search.mode = SearchMode::Sentence;
        search.query = "猫 猫".into();
        search.tokens = vec![
            SentenceToken { range: 0..3, text: "猫".into(), selectable: true },
            SentenceToken { range: 3..4, text: " ".into(), selectable: false },
            SentenceToken { range: 4..7, text: "猫".into(), selectable: true },
        ];
        let _ = update(&mut search, Message::Word(1));
        assert!(receiver.try_recv().is_err());
        let _ = update(&mut search, Message::Word(2));
        assert_eq!(search.selected, Some(2));
        assert_eq!(search.tokens[search.selected.unwrap()].range, 4..7);
        assert_eq!(receiver.recv().unwrap().0.query, "猫");
        assert_eq!(search.query, "猫 猫");
    }

    #[test]
    fn changing_sentence_words_retires_stale_definition_hover() {
        let (mut search, receiver) = fixture();
        search.mode = SearchMode::Sentence;
        search.query = "猫 犬".into();
        search.tokens = vec![
            SentenceToken { range: 0..3, text: "猫".into(), selectable: true },
            SentenceToken { range: 3..4, text: " ".into(), selectable: false },
            SentenceToken { range: 4..7, text: "犬".into(), selectable: true },
        ];
        let id = window::Id::unique();
        let mut definition = Definition::new(
            crate::popup::canned(), &search.config, &search.theme,
            &mut search.engine, search.media.as_mut(),
        ).unwrap();
        definition.hovered = Some("食べる".into());
        definition.generation = 7;
        search.definitions.push((id, definition));
        let hover = Target::Hover(id, 7);
        let _ = search.enqueue(Job {
            target: hover.clone(), query: "食べる".into(), tokenize: false,
        });
        assert_eq!(receiver.recv().unwrap().0.query, "食べる");

        let _ = update(&mut search, Message::Word(2));
        assert!(search.definitions.is_empty());
        assert!(receiver.try_recv().is_err());
        let _ = update(&mut search, Message::Finished(hover, Ok(Box::new(Reply {
            result: SearchResult::Found(Box::new(crate::popup::canned())),
            tokens: Vec::new(), config: Config::default(), database: PathBuf::new(),
        }))));
        assert!(search.definitions.is_empty());
        assert_eq!(receiver.recv().unwrap().0.query, "犬");
    }

    #[test]
    fn closed_definition_events_cannot_reacquire_search_focus() {
        let (mut search, _) = fixture();
        let unknown = window::Id::unique();
        let _ = update(&mut search, Message::Presence(unknown, true));
        let _ = update(&mut search, Message::Window(unknown, window::Event::Focused));
        assert!(search.hovered.is_empty());
        assert!(search.focused.is_empty());
        assert!(search.focus.is_none());
    }
}
