use crate::paths::Paths;
use crate::popup::text::TextEngine;
use crate::search_popup::Definition;
use anyhow::{Context, Result};
use chibipop::config::{Config, FrequencyConfig, ProfileSession};
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::search::{candidates, selected_presentation, SearchIdentity, SearchMode, SearchResult,
    SearchService, SentenceToken};
use chibipop::ui::theme::Theme;
use chibipop_linux::media::MediaSurfaces;
use iced::widget::{button, column, container, image, mouse_area, rich_text, row,
    scrollable, span, text, text_editor, text_input};
use iced::futures::SinkExt;
use iced::{Color, Element, Length, Point, Size, Task, font, window};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, mpsc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

pub const PROFILE_CATALOG_LIMIT: u64 = 1024 * 1024;
pub const MAX_SEARCH_TEXT_BYTES: usize = 64 * 1024;
const PROFILE_CATALOG_DIRECTORY: &str = "search-catalog";
const SEARCH_IPC_DIRECTORY: &str = "search-ipc";
const MAX_DEFINITIONS: usize = 8;
const RESOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const IPC_STARTUP_TIMEOUT: Duration = Duration::from_secs(2);
const IPC_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const IPC_READ_TIMEOUT: Duration = Duration::from_millis(100);
const IPC_ACTIVATE: u8 = 0;
const IPC_ACTIVATE_WITH_TEXT: u8 = 1;
const IPC_INVALIDATE: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
enum IpcRequest {
    Activate(Option<String>),
    Invalidate,
}

fn validate_search_text(text: &str) -> io::Result<()> {
    if text.len() > MAX_SEARCH_TEXT_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("search text exceeds {MAX_SEARCH_TEXT_BYTES} bytes")));
    }
    Ok(())
}

fn encode_activation(text: Option<&str>) -> io::Result<Vec<u8>> {
    if let Some(text) = text {
        validate_search_text(text)?;
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.push(IPC_ACTIVATE_WITH_TEXT);
        bytes.extend_from_slice(text.as_bytes());
        Ok(bytes)
    } else {
        Ok(vec![IPC_ACTIVATE])
    }
}

fn decode_ipc_request(bytes: &[u8]) -> Option<IpcRequest> {
    match bytes {
        [IPC_ACTIVATE] => Some(IpcRequest::Activate(None)),
        [IPC_INVALIDATE] => Some(IpcRequest::Invalidate),
        [IPC_ACTIVATE_WITH_TEXT, text @ ..] if text.len() <= MAX_SEARCH_TEXT_BYTES => {
            String::from_utf8(text.to_vec()).ok().map(|text| IpcRequest::Activate(Some(text)))
        }
        _ => None,
    }
}

fn private_search_ipc_directory(paths: &Paths) -> io::Result<PathBuf> {
    let runtime = paths.runtime_dir().map_err(io::Error::other)?;
    fs::create_dir_all(runtime)?;
    let directory = runtime.join(SEARCH_IPC_DIRECTORY);
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let runtime_metadata = fs::metadata(runtime)?;
    let owner = fs::metadata("/proc/self")?.uid();
    let metadata = fs::symlink_metadata(&directory)?;
    if runtime_metadata.uid() != owner || !metadata.file_type().is_dir() || metadata.uid() != owner {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "search IPC directory is not a directory owned by this user"));
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

fn search_identity_key(identity: &SearchIdentity, display: &str) -> String {
    let mut digest = Sha256::new();
    digest.update([match identity.mode {
        SearchMode::Dictionary => 0,
        SearchMode::Sentence => 1,
    }]);
    digest.update((identity.profile_id.len() as u64).to_le_bytes());
    digest.update(identity.profile_id.as_bytes());
    digest.update((display.len() as u64).to_le_bytes());
    digest.update(display.as_bytes());
    let digest = digest.finalize();
    let mut key = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn search_lock_name(identity: &SearchIdentity, display: &str) -> String {
    format!("search-{}.lock", search_identity_key(identity, display))
}

fn search_ipc_path(paths: &Paths, identity: &SearchIdentity, display: &str) -> io::Result<PathBuf> {
    Ok(private_search_ipc_directory(paths)?
        .join(format!("{}.sock", search_identity_key(identity, display))))
}

fn send_ipc_request(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let socket = UnixDatagram::unbound()?;
    let sent = socket.send_to(bytes, path)?;
    if sent != bytes.len() {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "search IPC sent a partial datagram"));
    }
    Ok(())
}

pub(crate) fn activate_existing(
    paths: &Paths,
    identity: &SearchIdentity,
    text: Option<&str>,
) -> io::Result<()> {
    let bytes = encode_activation(text)?;
    let display = crate::wayland::display_name().map_err(io::Error::other)?;
    let path = search_ipc_path(paths, identity, &display)?;
    let deadline = Instant::now() + IPC_STARTUP_TIMEOUT;
    loop {
        match send_ipc_request(&path, &bytes) {
            Ok(()) => return Ok(()),
            Err(error) if matches!(error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock)
                && Instant::now() < deadline => thread::sleep(IPC_RETRY_INTERVAL),
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn invalidate_existing(paths: &Paths, identity: &SearchIdentity) -> io::Result<()> {
    let display = crate::wayland::display_name().map_err(io::Error::other)?;
    let path = search_ipc_path(paths, identity, &display)?;
    send_ipc_request(&path, &[IPC_INVALIDATE])
}

struct SearchIpcServer {
    path: PathBuf,
    socket: Arc<UnixDatagram>,
}

impl SearchIpcServer {
    fn bind(paths: &Paths, identity: &SearchIdentity, display: &str) -> io::Result<Self> {
        let path = search_ipc_path(paths, identity, display)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = UnixDatagram::bind(&path)?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .and_then(|()| socket.set_read_timeout(Some(IPC_READ_TIMEOUT)))
        {
            drop(socket);
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self { path, socket: Arc::new(socket) })
    }

    fn subscription(self: &Arc<Self>) -> iced::Subscription<Message> {
        iced::Subscription::run_with(
            IpcSubscriptionData { key: self.path.as_os_str().to_string_lossy().into_owned(),
                server: Arc::clone(self) },
            ipc_stream,
        )
    }
}

impl Drop for SearchIpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct IpcSubscriptionData {
    key: String,
    server: Arc<SearchIpcServer>,
}

impl Hash for IpcSubscriptionData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

fn ipc_stream(data: &IpcSubscriptionData) -> impl iced::futures::Stream<Item = Message> + 'static {
    let socket = data.server.socket.try_clone();
    iced::stream::channel(16, async move |mut output| {
        let socket = match socket {
            Ok(socket) => socket,
            Err(error) => {
                let _ = output.send(Message::IpcFailure(error.to_string())).await;
                return;
            }
        };
        let mut thread_output = output.clone();
        let listener = thread::Builder::new().name("search-ipc".into()).spawn(move || {
            let mut bytes = [0u8; MAX_SEARCH_TEXT_BYTES + 2];
            let mut next_resource_check = Instant::now() + RESOURCE_CHECK_INTERVAL;
            loop {
                match socket.recv(&mut bytes) {
                    Ok(length) => {
                        if let Some(request) = decode_ipc_request(&bytes[..length]) {
                            if iced::futures::executor::block_on(
                                thread_output.send(Message::Ipc(request)),
                            ).is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) if matches!(error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = iced::futures::executor::block_on(
                            thread_output.send(Message::IpcFailure(error.to_string())),
                        );
                        break;
                    }
                }
                if Instant::now() >= next_resource_check {
                    if iced::futures::executor::block_on(thread_output.send(Message::CheckResources)).is_err() {
                        break;
                    }
                    next_resource_check = Instant::now() + RESOURCE_CHECK_INTERVAL;
                }
            }
        });
        if let Err(error) = listener {
            let _ = output.send(Message::IpcFailure(error.to_string())).await;
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSignature {
    length: u64,
    modified: SystemTime,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(FileSignature {
        length: metadata.len(),
        modified: metadata.modified().ok()?,
        device: metadata.dev(),
        inode: metadata.ino(),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

#[derive(Clone, Debug, PartialEq)]
struct SharedPolicy {
    frequency: FrequencyConfig,
    plugins: Vec<String>,
}

impl From<&Config> for SharedPolicy {
    fn from(config: &Config) -> Self {
        Self {
            frequency: config.dictionaries.clone(),
            plugins: config.plugins.enabled.clone(),
        }
    }
}

fn normalized_shared_policy(config: &Config, dicts: &[chibipop::present::DictInfo]) -> SharedPolicy {
    let mut normalized = config.clone();
    normalized.migrate_dictionary_lists(dicts);
    SharedPolicy::from(&normalized)
}

fn read_config(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Config::from_toml(&text).with_context(|| format!("parsing {}", path.display()))
}

#[derive(Clone)]
struct ResourceWatch {
    database: PathBuf,
    config: PathBuf,
    database_signature: Option<FileSignature>,
    config_signature: Option<FileSignature>,
    policy: SharedPolicy,
    checked_at: Option<Instant>,
}

impl ResourceWatch {
    fn new(database: PathBuf, config: PathBuf, database_signature: Option<FileSignature>,
        session: &ProfileSession) -> Self {
        Self {
            database,
            config,
            database_signature,
            config_signature: None,
            policy: SharedPolicy::from(&session.catalog.config),
            checked_at: None,
        }
    }

    fn changed(&mut self) -> Result<bool> {
        let now = Instant::now();
        if self.checked_at.is_some_and(|last| now.duration_since(last) < RESOURCE_CHECK_INTERVAL) {
            return Ok(false);
        }
        self.checked_at = Some(now);

        let database_signature = file_signature(&self.database);
        if database_signature != self.database_signature {
            self.database_signature = database_signature;
            return Ok(true);
        }

        let config_signature = file_signature(&self.config);
        if config_signature == self.config_signature {
            return Ok(false);
        }
        let Some(config_signature) = config_signature else {
            return Ok(self.config_signature.take().is_some());
        };

        let saved = read_config(&self.config)?;
        let raw_policy = SharedPolicy::from(&saved);
        if raw_policy.plugins != self.policy.plugins {
            return Ok(true);
        }
        if raw_policy.frequency != self.policy.frequency {
            let dictionary = SqliteDictionary::open(&self.database)
                .context("opening the dictionary to normalize shared Search resources")?;
            let dicts = dictionary.dicts()
                .context("reading dictionary identities to normalize shared Search resources")?;
            if normalized_shared_policy(&saved, &dicts) != self.policy {
                return Ok(true);
            }
        }
        self.config_signature = Some(config_signature);
        Ok(false)
    }
}


pub struct SearchCommand {
    command: Command,
    catalog_path: PathBuf,
}

impl SearchCommand {
    pub fn command_mut(&mut self) -> &mut Command { &mut self.command }

    pub fn catalog_path(&self) -> &Path { &self.catalog_path }

    pub fn remove_catalog(&self) -> io::Result<()> {
        match fs::remove_file(&self.catalog_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn focus_path(paths: &Paths) -> Result<PathBuf> {
    let display = crate::wayland::display_name()?;
    Ok(paths.runtime_dir()?.join(format!("search-focus-{}.lock", crate::lock::sanitize(&display))))
}

pub fn is_focused(paths: &Paths) -> Result<bool> {
    focus::is_held(&focus_path(paths)?).context("probing search focus")
}

fn profile_catalog_directory(paths: &Paths) -> io::Result<PathBuf> {
    let runtime = paths.runtime_dir().map_err(io::Error::other)?;
    let directory = runtime.join(PROFILE_CATALOG_DIRECTORY);
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

fn write_snapshot(paths: &Paths, session: &ProfileSession) -> io::Result<PathBuf> {
    let serialized = session.catalog.config.to_toml().map_err(io::Error::other)?;
    if serialized.len() as u64 > PROFILE_CATALOG_LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "profile catalog exceeds 1 MiB"));
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = profile_catalog_directory(paths)?;
    for _ in 0..32 {
        let counter = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("search-catalog-{}-{counter}.toml", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(serialized.as_bytes()) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "cannot allocate a profile catalog snapshot"))
}

pub fn take_snapshot(paths: &Paths, path: &Path) -> Result<Config> {
    let directory = profile_catalog_directory(paths)?;
    anyhow::ensure!(path.parent() == Some(directory.as_path()),
        "profile catalog snapshot is outside its private directory");
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    anyhow::ensure!(name.starts_with("search-catalog-") && name.ends_with(".toml"),
        "profile catalog snapshot has an invalid name");
    let read = || -> Result<Vec<u8>> {
        let file = File::open(path).context("opening profile catalog snapshot")?;
        anyhow::ensure!(file.metadata()?.file_type().is_file(),
            "profile catalog snapshot is not a regular file");
        let mut bytes = Vec::new();
        file.take(PROFILE_CATALOG_LIMIT + 1).read_to_end(&mut bytes)
            .context("reading profile catalog snapshot")?;
        anyhow::ensure!(bytes.len() as u64 <= PROFILE_CATALOG_LIMIT,
            "profile catalog snapshot exceeds 1 MiB");
        Ok(bytes)
    };
    let bytes = read();
    let removed = fs::remove_file(path).context("removing profile catalog snapshot");
    let bytes = bytes?;
    removed?;
    let text = std::str::from_utf8(&bytes).context("profile catalog snapshot is not UTF-8")?;
    Config::from_toml(text).context("parsing profile catalog snapshot")
}

fn search_command_base(paths: &Paths, mode: SearchMode, session: &ProfileSession)
    -> io::Result<SearchCommand> {
    let executable = std::env::current_exe()?;
    let catalog_path = write_snapshot(paths, session)?;
    let mut command = Command::new(executable);
    command.arg("--config").arg(&paths.config_file).arg(match mode {
        SearchMode::Dictionary => "search", SearchMode::Sentence => "sentence-search",
    }).arg("--data-dir").arg(&paths.data_dir)
        .arg("--profile-catalog").arg(&catalog_path)
        .arg("--profile-id").arg(session.id());
    crate::signals::unmasked(&mut command);
    Ok(SearchCommand { command, catalog_path })
}

fn popup_css_path(paths: &Paths) -> PathBuf {
    paths.config_file.with_file_name("popup.css")
}

pub fn search_command_mode(paths: &Paths, mode: SearchMode, session: &ProfileSession)
    -> io::Result<SearchCommand> {
    search_command_base(paths, mode, session)
}

pub(crate) fn search_stdin_command_mode(paths: &Paths, mode: SearchMode, session: &ProfileSession)
    -> io::Result<SearchCommand> {
    let mut launch = search_command_base(paths, mode, session)?;
    launch.command_mut().arg("--read-stdin").stdin(Stdio::piped());
    Ok(launch)
}
pub fn run(paths: Paths, mode: SearchMode, initial: Option<String>, saved: Config, profile_id: String) -> Result<()> {
    if let Some(text) = &initial {
        validate_search_text(text)?;
    }
    let display = crate::wayland::display_name()?;
    let identity = SearchIdentity::new(mode, profile_id);
    let lock_name = search_lock_name(&identity, &display);
    let _instance = match crate::lock::acquire_at(paths.runtime_dir()?, &lock_name) {
        Ok(lock) => lock,
        Err(crate::lock::LockError::AlreadyRunning { .. }) => {
            activate_existing(&paths, &identity, initial.as_deref())?;
            return Ok(());
        }
        Err(crate::lock::LockError::Io(error)) => return Err(error.into()),
    };
    let database = paths.data_dir.join("chibipop.sqlite");
    let database_signature = file_signature(&database);
    let ipc = Arc::new(SearchIpcServer::bind(&paths, &identity, &display)?);
    let (worker, session) = Worker::new(
        database.clone(), data_file("data/deconjugator.json"), saved, identity.profile_id.clone(),
    )?;
    let resource_watch = ResourceWatch::new(
        database.clone(), paths.config_file.clone(), database_signature, &session,
    );
    let css_path = popup_css_path(&paths);
    let focus_path = focus_path(&paths)?;
    focus::prepare(&focus_path).context("Cannot create the Search focus lock")?;
    let worker = Arc::new(worker);
    let config = session.config().clone();
    let initial = initial.unwrap_or_default();
    let mut engine = TextEngine::new(&config.popup.font);
    let theme = crate::search_popup::theme(&config, Some(&css_path), &mut engine);
    let font = search_font(&theme.font_name);
    iced::daemon(move || {
        let (main, open) = window::open(window::Settings {
            size: Size::new(760.0, 640.0), min_size: Some(Size::new(340.0, 260.0)),
            transparent: true,
            ..window::Settings::default()
        });
        let mut engine = TextEngine::new(&config.popup.font);
        let theme = crate::search_popup::theme(&config, Some(&css_path), &mut engine);
        let media = MediaSurfaces::open(&database).ok();
        let mut search = Search {
            mode: identity.mode, query: initial.clone(), editor: text_editor::Content::with_text(&initial),
            tokens: Vec::new(), selected: None, result: SearchResult::Empty, status: String::new(),
            generation: 0, worker: worker.clone(), busy: false, pending: None,
            main, definitions: Vec::new(), session: session.clone(),
            css_path: css_path.clone(),
            theme, font, engine, media,
            focused: HashSet::new(), hovered: HashSet::new(),
            focus_path: focus_path.clone(), focus: None,
            resource_watch: resource_watch.clone(), ipc: Some(ipc.clone()),
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
        .subscription(|search: &Search| {
            let events = iced::event::listen_with(|event, _, id| match event {
                iced::Event::Window(event) => Some(Message::Window(id, event)),
                iced::Event::Mouse(iced::mouse::Event::CursorEntered) => Some(Message::Presence(id, true)),
                iced::Event::Mouse(iced::mouse::Event::CursorLeft) => Some(Message::Presence(id, false)),
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), ..
                }) => Some(Message::Back(id)),
                _ => None,
            });
            match &search.ipc {
                Some(ipc) => iced::Subscription::batch([events, ipc.subscription()]),
                None => events,
            }
        })
        .run().context("running search")
}

fn title(mode: SearchMode) -> &'static str {
    match mode { SearchMode::Dictionary => "Dictionary search", SearchMode::Sentence => "Sentence search" }
}

static SEARCH_FONTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let mut names: Vec<_> = database.faces().map(|face| face.families[0].0.clone()).collect();
    names.sort();
    names.dedup();
    names
});

fn search_font(name: &str) -> iced::Font {
    SEARCH_FONTS.iter().find(|family| family.as_str() == name)
        .map_or(iced::Font::DEFAULT, |family| iced::Font::with_name(family.as_str()))
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
    session: ProfileSession,
    css_path: PathBuf,
    theme: Theme,
    font: iced::Font,
    engine: TextEngine,
    media: Option<MediaSurfaces>,
    focus_path: PathBuf,
    focus: Option<focus::Lease>,
    focused: HashSet<window::Id>,
    hovered: HashSet<window::Id>,
    resource_watch: ResourceWatch,
    ipc: Option<Arc<SearchIpcServer>>,
}

#[derive(Clone, Debug)]
enum Target { Input(u64), Word(u64), Hover(window::Id, u64) }

#[derive(Clone, Debug)]
struct Job { target: Target, query: String, tokenize: bool, session: ProfileSession }

#[derive(Clone, Debug)]
struct Reply { result: SearchResult, tokens: Vec<SentenceToken>, session: ProfileSession }

#[derive(Clone, Debug)]
enum Message {
    Input(String), Edit(text_editor::Action), Submit, Word(usize), Candidate(usize),
    Finished(Target, std::result::Result<Box<Reply>, String>), Opened(window::Id),
    Window(window::Id, window::Event), Presence(window::Id, bool),
    Hover(window::Id, Point), Leave(window::Id), Scroll(window::Id, iced::mouse::ScrollDelta),
    Scale(window::Id, f32), Back(window::Id), Click(window::Id),
    Ipc(IpcRequest), CheckResources, IpcFailure(String),
}

type WorkerReply = iced::futures::channel::oneshot::Sender<std::result::Result<Box<Reply>, String>>;
struct Worker { sender: mpsc::Sender<(Job, WorkerReply)> }

impl Worker {
    fn new(database: PathBuf, deconjugator: PathBuf, saved: Config, profile_id: String)
        -> Result<(Self, ProfileSession)> {
        let (sender, receiver) = mpsc::channel::<(Job, WorkerReply)>();
        let (startup_sender, startup_receiver) = mpsc::channel::<Result<ProfileSession>>();
        thread::Builder::new().name("search-worker".into()).spawn(move || {
            let (service, session) = match SearchService::open_catalog(
                &database, &deconjugator, &saved, &profile_id,
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = startup_sender.send(Err(error));
                    return;
                }
            };
            if startup_sender.send(Ok(session.clone())).is_err() {
                return;
            }
            for (job, reply) in receiver {
                let run = || -> Result<Reply> {
                    let tokens = if job.tokenize {
                        service.sentence_tokens_in(&job.session, &job.query)?
                    } else { Vec::new() };
                    let result = if job.tokenize || job.query.trim().is_empty() {
                        SearchResult::Empty
                    } else if matches!(job.target, Target::Word(_)) {
                        service.search_word_in(&job.session, &job.query)?
                    } else {
                        service.search_in(&job.session, &job.query)?
                    };
                    Ok(Reply { result, tokens, session: job.session.clone() })
                };
                let _ = reply.send(run().map(Box::new).map_err(|error| format!("Search failed: {error:#}")));
            }
        })?;
        let session = startup_receiver.recv().context("search worker ended before opening its catalog")??;
        Ok((Self { sender }, session))
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
        self.status = if self.query.trim().is_empty() { "Type or paste text." } else { "Searching…" }.into();
        let close = self.close_from(0);
        let query = self.enqueue(Job { target: Target::Input(self.generation), query: self.query.clone(),
            tokenize: self.mode == SearchMode::Sentence, session: self.session.clone() });
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

    fn cancel_hovers(&mut self) {
        if self.pending.as_ref().is_some_and(|job| matches!(job.target, Target::Hover(_, _))) {
            self.pending = None;
        }
        for (_, definition) in &mut self.definitions {
            definition.hovered = None;
            definition.generation = definition.generation.wrapping_add(1);
        }
    }

    fn cancel_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.cancel_hovers();
    }

    fn open_definition(&mut self, presentation: chibipop::present::Presentation,
        session: ProfileSession) -> Task<Message> {
        if self.definitions.len() >= MAX_DEFINITIONS {
            self.status = "Close a definition window before opening another level.".into();
            return Task::none();
        }
        let theme = crate::search_popup::theme(session.config(), Some(&self.css_path), &mut self.engine);
        match Definition::new(presentation, session, theme, &mut self.engine, self.media.as_mut()) {
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
        Message::Ipc(IpcRequest::Activate(text)) => {
            let search_query = if let Some(text) = text {
                search.query = text.clone();
                search.editor = text_editor::Content::with_text(&text);
                search.input_changed()
            } else {
                Task::none()
            };
            return Task::batch([
                window::gain_focus(search.main),
                iced::widget::operation::focus("search-query"),
                search_query,
            ]);
        }
        Message::Ipc(IpcRequest::Invalidate) => {
            search.cancel_all();
            return Task::batch([search.close_from(0), iced::exit()]);
        }
        Message::CheckResources => match search.resource_watch.changed() {
            Ok(true) => {
                search.cancel_all();
                return Task::batch([search.close_from(0), iced::exit()]);
            }
            Ok(false) => {}
            Err(error) => search.status = format!("Cannot check shared Search resources: {error:#}"),
        },
        Message::IpcFailure(error) => {
            search.status = format!("Search activation transport failed: {error}");
        }
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
                session: search.session.clone(),
            });
            return Task::batch([close, lookup]);
        }
        Message::Candidate(index) => {
            if let Some(presentation) = selected_presentation(&search.result, index) {
                let close = search.close_from(0);
                return Task::batch([close, search.open_definition(presentation, search.session.clone())]);
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
                    Ok(reply) => {
                        let Reply { result, tokens, session } = *reply;
                        match target {
                            Target::Input(_) | Target::Word(_) => {
                                if matches!(target, Target::Input(_)) { search.tokens = tokens; }
                                search.result = result;
                                search.status = match search.result {
                                    SearchResult::Found(_) => "Choose a candidate to open its definition.",
                                    SearchResult::Miss => "No matching entries in the enabled dictionaries.",
                                    SearchResult::Empty if search.mode == SearchMode::Sentence => "Paste a sentence, then click a word to see its candidates.",
                                    SearchResult::Empty => "Type or paste a word or expression.",
                                }.into();
                            }
                            Target::Hover(id, _) => {
                                if let Some(presentation) = selected_presentation(&result, 0) {
                                    if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                                        tasks.push(search.close_from(depth + 1));
                                        tasks.push(search.open_definition(presentation, session));
                                    }
                                }
                            }
                        }
                    }
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
                    if let Err(error) = definition.resize(&mut search.engine, search.media.as_mut(),
                        size, definition.scale) { search.status = error.to_string(); }
                }
            }
            window::Event::Rescaled(scale) => return update(search, Message::Scale(id, scale)),
            _ => {}
        },
        Message::Scale(id, scale) => {
            if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                if let Err(error) = definition.resize(&mut search.engine, search.media.as_mut(),
                    definition.size, scale) { search.status = error.to_string(); }
            }
        }
        Message::Hover(id, point) => {
            let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) else { return Task::none() };
            let definition = &mut search.definitions[depth].1;
            definition.pointer = point;
            if !definition.session.config().popup.sub_popups { return Task::none(); }
            let query = definition.hover(point, &mut search.engine);
            if query == definition.hovered { return Task::none(); }
            definition.hovered = query.clone();
            definition.generation = definition.generation.wrapping_add(1);
            let generation = definition.generation;
            let session = definition.session.nested();
            if let Some(query) = query {
                let close = search.close_from(depth + 1);
                let lookup = search.enqueue(Job {
                    target: Target::Hover(id, generation), query, tokenize: false, session,
                });
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
                search.cancel_hovers();
                return Task::batch([search.close_from(depth), window::gain_focus(parent)]);
            }
            if id == search.main {
                search.cancel_all();
                return Task::batch([search.close_from(0), iced::exit()]);
            }
        }
        Message::Click(id) => {
            if let Some((_, definition)) = search.definitions.iter().find(|(known, _)| *known == id) {
                match definition.click() {
                    Some(chibipop::controller::HitAction::Back) => return update(search, Message::Back(id)),
                    Some(chibipop::controller::HitAction::DrillDown(query)) => {
                        let session = definition.session.nested();
                        let definition = &mut search.definitions.iter_mut().find(|(known, _)| *known == id).expect("existing definition").1;
                        definition.hovered = Some(query.clone());
                        definition.generation = definition.generation.wrapping_add(1);
                        let target = Target::Hover(id, definition.generation);
                        return search.enqueue(Job { target, query, tokenize: false, session });
                    }
                    _ => {}
                }
            }
        }
        Message::Scroll(id, delta) => {
            if let Some((_, definition)) = search.definitions.iter_mut().find(|(known, _)| *known == id) {
                let delta = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y * definition.body_size() * 3.0,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                definition.scroll -= delta * definition.scale;
                definition.hovered = None;
                definition.generation = definition.generation.wrapping_add(1);
                if let Err(error) = definition.paint(&mut search.engine, search.media.as_mut()) {
                    search.status = error.to_string();
                }
            }
        }
    }
    Task::none()
}

fn color(rgb: (u8, u8, u8)) -> Color { Color::from_rgb8(rgb.0, rgb.1, rgb.2) }

fn panel_color(rgb: (u8, u8, u8), opacity: f32) -> Color {
    Color { a: opacity.clamp(0.0, 1.0), ..color(rgb) }
}

fn themed_font(base: iced::Font, weight: u16, italic: bool) -> iced::Font {
    let weight = match weight {
        ..=149 => font::Weight::Thin,
        150..=249 => font::Weight::ExtraLight,
        250..=349 => font::Weight::Light,
        350..=449 => font::Weight::Normal,
        450..=549 => font::Weight::Medium,
        550..=649 => font::Weight::Semibold,
        650..=749 => font::Weight::Bold,
        750..=849 => font::Weight::ExtraBold,
        _ => font::Weight::Black,
    };
    let style = if italic { font::Style::Italic } else { font::Style::Normal };
    iced::Font { weight, style, ..base }
}

fn sentence_size(theme: &Theme) -> f32 {
    (theme.body_size * 1.35)
        .max(theme.headword_size + 2.0)
        .max(theme.collapsed_size + 2.0)
}

fn border(theme: &Theme) -> iced::Border {
    iced::Border { color: panel_color(theme.border, theme.opacity), width: theme.border_width.max(1.0),
        radius: (theme.corner_radius as f32).into() }
}

fn search_button(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(panel_color(if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            theme.separator
        } else { theme.background }, theme.opacity).into()),
        text_color: color(theme.body_text), border: border(theme), ..button::Style::default()
    }
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
    let body_font = themed_font(search.font, theme.body_weight, theme.body_italic);
    let headword_font = themed_font(search.font, theme.headword_weight, theme.headword_italic);
    let reading_font = themed_font(search.font, theme.reading_weight, theme.reading_italic);
    let collapsed_font = themed_font(search.font, theme.collapsed_weight, theme.collapsed_italic);
    let dimmed_font = themed_font(search.font, theme.dimmed_weight, theme.dimmed_italic);
    let input: Element<'_, Message> = match search.mode {
        SearchMode::Dictionary => text_input("Word or expression", &search.query)
            .id("search-query").on_input(Message::Input).on_submit(Message::Submit)
            .font(body_font).size(theme.body_size).padding(pad).style(move |_, _| text_input::Style {
                background: panel_color(theme.background, theme.opacity).into(), border: border(theme),
                icon: color(theme.body_text), placeholder: color(theme.dimmed_text),
                value: color(theme.body_text), selection: color(theme.accent),
            }).into(),
        SearchMode::Sentence => text_editor(&search.editor).id("search-query").on_action(Message::Edit)
            .font(body_font).size(sentence_size(theme)).padding(pad).height(140).style(move |_, _| text_editor::Style {
                background: panel_color(theme.background, theme.opacity).into(), border: border(theme),
                placeholder: color(theme.dimmed_text), value: color(theme.body_text), selection: color(theme.accent),
            }).into(),
    };
    let label = |value| text(value).font(dimmed_font).size(theme.dimmed_size).color(color(theme.dimmed_text));
    let input_label = if search.mode == SearchMode::Sentence { "Sentence" } else { "Word or expression" };
    let submit_label = if search.mode == SearchMode::Sentence { "Update sentence" } else { "Search" };
    let action = |value| button(text(value).font(body_font).size(theme.body_size)
        .width(Length::Fill).align_x(iced::Center)).padding(pad).width(Length::Fill)
        .style(move |_, status| search_button(theme, status));
    let mut content = column![text(title(search.mode)).font(headword_font).size(theme.headword_size)
        .color(color(theme.headword_text)), label(input_label), input,
        row![action(submit_label).on_press(Message::Submit), action("Close").on_press(Message::Back(search.main))]
            .spacing(pad)].spacing(pad);
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
        content = content.push(label("Select a word"))
            .push(container(scrollable(rich_text(spans).font(body_font).size(sentence_size(theme))
                .on_link_click(Message::Word)).height(100)).width(Length::Fill).padding(pad)
                .style(move |_| container::Style { border: border(theme), ..container::Style::default() }));
    }
    content = content.push(text(&search.status).font(dimmed_font).size(theme.dimmed_size).color(color(theme.dimmed_text)));
    content = content.push(label("Matching words"));
    let mut rows = column![].spacing(pad);
    for candidate in candidates(&search.result) {
        let heading = row![text(candidate.headword).font(headword_font).size(theme.headword_size).color(color(theme.headword_text)),
            text(candidate.reading).font(reading_font).size(theme.reading_size).color(color(theme.reading_text))]
            .spacing(pad).align_y(iced::Center);
        let label = column![heading, text(candidate.summary).font(collapsed_font)
            .size(theme.collapsed_size).color(color(theme.collapsed_text))]
            .spacing(pad / 2.0).width(Length::Fill).align_x(iced::Center);
        rows = rows.push(button(label).width(Length::Fill).padding(pad)
            .style(move |_, status| search_button(theme, status)).on_press(Message::Candidate(candidate.index)));
    }
    container(content.push(scrollable(rows).height(Length::Fill))).padding(pad)
        .style(move |_| container::Style {
            background: Some(panel_color(theme.background, theme.opacity).into()),
            text_color: Some(color(theme.body_text)), border: border(theme),
            ..container::Style::default()
        }).into()
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

    fn test_session() -> ProfileSession {
        let saved = Config::default();
        chibipop::config::ProfileCatalog::new(&saved, &[]).unwrap().session(None).unwrap()
    }

    fn fixture() -> (Search, mpsc::Receiver<(Job, WorkerReply)>) {
        let (sender, receiver) = mpsc::channel();
        let session = test_session();
        let config = session.config().clone();
        let mut engine = TextEngine::new("Noto Sans CJK JP");
        let theme = crate::search_popup::theme(&config, None, &mut engine);
        let resource_watch = ResourceWatch::new(PathBuf::new(), PathBuf::new(), None, &session);
        (Search {
            mode: SearchMode::Dictionary, query: String::new(), editor: text_editor::Content::new(),
            tokens: Vec::new(), selected: None, result: SearchResult::Empty, status: String::new(),
            generation: 0, worker: std::sync::Arc::new(Worker { sender }), busy: false, pending: None,
            main: window::Id::unique(), definitions: Vec::new(), session,
            css_path: PathBuf::new(),
            theme, font: iced::Font::DEFAULT, engine, media: None,
            focus_path: PathBuf::new(), focus: None, focused: HashSet::new(), hovered: HashSet::new(),
            resource_watch, ipc: None,
        }, receiver)
    }

    struct RuntimeDir(PathBuf);
    impl RuntimeDir {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("chibipop-search-snapshot-{stamp}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn short() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = PathBuf::from("/tmp").join(format!(
                "c{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for RuntimeDir { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn command_paths(runtime: &Path) -> Paths {
        let mut paths = crate::paths::resolve(
            &crate::paths::Env::from_process(), Some("/tmp/search config.toml".into()),
        );
        paths.data_dir = "/tmp/portable dictionary".into();
        paths.runtime_dir = Some(runtime.to_path_buf());
        paths
    }

    fn write_saved_config(path: &Path, config: &Config) {
        let temporary = path.with_extension("next.toml");
        fs::write(&temporary, config.to_toml().unwrap()).unwrap();
        fs::rename(temporary, path).unwrap();
    }

    fn resource_watch_fixture(runtime: &RuntimeDir) -> (ResourceWatch, PathBuf, PathBuf, Config) {
        let database = runtime.0.join("chibipop.sqlite");
        let config_path = runtime.0.join("config.toml");
        let saved = Config::default();
        fs::write(&database, b"dictionary").unwrap();
        write_saved_config(&config_path, &saved);
        let session = chibipop::config::ProfileCatalog::new(&saved, &[]).unwrap()
            .session(None).unwrap();
        let mut watch = ResourceWatch::new(
            database.clone(), config_path.clone(), file_signature(&database), &session,
        );
        watch.config_signature = file_signature(&config_path);
        (watch, database, config_path, saved)
    }


    #[test]
    fn child_commands_carry_profile_catalog_without_query_argv() {
        let runtime = RuntimeDir::new();
        let paths = command_paths(&runtime.0);
        let session = test_session();
        let mut command = search_command_mode(&paths, SearchMode::Sentence, &session).unwrap();
        let args: Vec<_> = command.command_mut().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let strings: Vec<_> = args.iter().map(String::as_str).collect();
        assert_eq!(&strings[..6], ["--config", "/tmp/search config.toml", "sentence-search",
            "--data-dir", "/tmp/portable dictionary", "--profile-catalog"]);
        assert_eq!(args[6], command.catalog_path().to_string_lossy().into_owned());
        assert_eq!(&strings[7..], ["--profile-id", session.id()]);
        assert!(!args.iter().any(|arg| arg == "--text" || arg == "猫"));
        assert_eq!(take_snapshot(&paths, command.catalog_path()).unwrap(), session.catalog.config);
        command.remove_catalog().unwrap();

        let mut command = search_command_mode(&paths, SearchMode::Dictionary, &session).unwrap();
        let args: Vec<_> = command.command_mut().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args[2], "search");
        assert!(!args.iter().any(|arg| arg == "--text"));
        command.remove_catalog().unwrap();
        assert_eq!(popup_css_path(&paths), PathBuf::from("/tmp/popup.css"));
    }

    #[test]
    fn captured_text_command_uses_stdin_without_query_argv() {
        let runtime = RuntimeDir::new();
        let paths = command_paths(&runtime.0);
        let session = test_session();
        let mut command = search_stdin_command_mode(&paths, SearchMode::Sentence, &session).unwrap();
        let args: Vec<_> = command.command_mut().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args[2], "sentence-search");
        assert!(args.iter().any(|arg| arg == "--read-stdin"));
        assert!(!args.iter().any(|arg| arg == "--text" || arg == "猫"));
        command.remove_catalog().unwrap();
    }
    #[test]
    fn activation_distinguishes_no_text_from_empty_text_and_enforces_the_byte_limit() {
        assert_eq!(decode_ipc_request(&encode_activation(None).unwrap()),
            Some(IpcRequest::Activate(None)));
        assert_eq!(decode_ipc_request(&encode_activation(Some("")).unwrap()),
            Some(IpcRequest::Activate(Some(String::new()))));

        let accepted = "x".repeat(MAX_SEARCH_TEXT_BYTES);
        let encoded = encode_activation(Some(&accepted)).unwrap();
        assert_eq!(decode_ipc_request(&encoded),
            Some(IpcRequest::Activate(Some(accepted))));
        assert!(encode_activation(Some(&"x".repeat(MAX_SEARCH_TEXT_BYTES + 1))).is_err());
        assert!(decode_ipc_request(&vec![IPC_ACTIVATE_WITH_TEXT; MAX_SEARCH_TEXT_BYTES + 2]).is_none());
        assert!(decode_ipc_request(&[IPC_ACTIVATE_WITH_TEXT, 0xff]).is_none());
        assert!(decode_ipc_request(&[IPC_ACTIVATE, 0]).is_none());
    }

    #[test]
    fn private_ipc_routes_per_mode_and_profile_with_owner_only_permissions() {
        let runtime = RuntimeDir::short();
        let paths = command_paths(&runtime.0);
        let identity = SearchIdentity::new(SearchMode::Dictionary, "p".repeat(64));
        let other_profile = SearchIdentity::new(SearchMode::Dictionary, "profile-2");
        let sentence = SearchIdentity::new(SearchMode::Sentence, "p".repeat(64));
        let path = search_ipc_path(&paths, &identity, "wayland-0").unwrap();
        assert!(path.to_string_lossy().len() <= 107);
        assert_ne!(path, search_ipc_path(&paths, &other_profile, "wayland-0").unwrap());
        assert_ne!(path, search_ipc_path(&paths, &sentence, "wayland-0").unwrap());

        let server = SearchIpcServer::bind(&paths, &identity, "wayland-0").unwrap();
        assert_eq!(0o700, fs::metadata(server.path.parent().unwrap()).unwrap().permissions().mode() & 0o777);
        assert_eq!(0o600, fs::metadata(&server.path).unwrap().permissions().mode() & 0o777);
        let mut bytes = [0u8; MAX_SEARCH_TEXT_BYTES + 2];
        send_ipc_request(&server.path, &encode_activation(None).unwrap()).unwrap();
        let length = server.socket.recv(&mut bytes).unwrap();
        assert_eq!(decode_ipc_request(&bytes[..length]), Some(IpcRequest::Activate(None)));
        send_ipc_request(&server.path, &encode_activation(Some("猫")).unwrap()).unwrap();
        let length = server.socket.recv(&mut bytes).unwrap();
        assert_eq!(decode_ipc_request(&bytes[..length]),
            Some(IpcRequest::Activate(Some("猫".into()))));
    }

    #[test]
    fn activation_keeps_the_existing_query_and_catalog_when_text_is_absent() {
        let (mut search, receiver) = fixture();
        let session = search.session.clone();
        search.query = "前の検索".into();
        search.editor = text_editor::Content::with_text(&search.query);

        let _ = update(&mut search, Message::Ipc(IpcRequest::Activate(None)));
        assert_eq!(search.query, "前の検索");
        assert_eq!(search.editor.text(), "前の検索");
        assert_eq!(search.session, session);
        assert!(receiver.try_recv().is_err());

        let _ = update(&mut search, Message::Ipc(IpcRequest::Activate(Some("新しい検索".into()))));
        assert_eq!(search.query, "新しい検索");
        assert_eq!(search.editor.text(), "新しい検索");
        let job = receiver.try_recv().unwrap().0;
        assert_eq!(job.query, "新しい検索");
        assert_eq!(job.session, session);
    }

    #[test]
    fn profile_and_default_edits_do_not_invalidate_an_open_search() {
        let runtime = RuntimeDir::new();
        let (mut watch, _, config_path, mut saved) = resource_watch_fixture(&runtime);
        saved.profiles[0].name = "Renamed profile".into();
        let id = saved.next_profile_id();
        let mut added = saved.profiles[0].clone();
        added.id = id.clone();
        added.name = "New default".into();
        saved.profiles.push(added);
        saved.default_profile = id;
        write_saved_config(&config_path, &saved);

        watch.checked_at = None;
        assert!(!watch.changed().unwrap());
    }

    #[test]
    fn shared_plugin_changes_invalidate_an_open_search() {
        let runtime = RuntimeDir::new();
        let (mut watch, _, config_path, saved) = resource_watch_fixture(&runtime);
        let mut changed = saved;
        changed.plugins.enabled.push("meikiocr".into());
        write_saved_config(&config_path, &changed);

        watch.checked_at = None;
        assert!(watch.changed().unwrap());
    }

    #[test]
    fn dictionary_file_changes_invalidate_an_open_search() {
        let runtime = RuntimeDir::new();
        let (mut watch, database, _, _) = resource_watch_fixture(&runtime);
        fs::write(database, b"rebuilt dictionary").unwrap();

        watch.checked_at = None;
        assert!(watch.changed().unwrap());
    }

    #[test]
    fn removing_the_saved_config_invalidates_an_open_search() {
        let runtime = RuntimeDir::new();
        let (mut watch, _, config_path, _) = resource_watch_fixture(&runtime);
        fs::remove_file(config_path).unwrap();

        watch.checked_at = None;
        assert!(watch.changed().unwrap());
    }

    #[test]
    fn frequency_policy_changes_survive_legacy_dictionary_normalization() {
        let text = "[trigger]\nmode = 'hold-shift'\n[popup]\n[dictionaries]\ndisplay_order = ['Jiten', 'Missing']\n[dictionaries.per_language]\nja = []\nzh = ['大辞']\n";
        let legacy = Config::from_toml(text).unwrap();
        let dicts = [chibipop::present::DictInfo { dict_id: 1, name: "Jitendex".into() }];
        let catalog = chibipop::config::ProfileCatalog::new(&legacy, &dicts).unwrap();
        assert_eq!(normalized_shared_policy(&legacy, &dicts), SharedPolicy::from(&catalog.config));

        let saved = Config::default();
        let mut changed = saved.clone();
        changed.dictionaries.frequency.push("Jitendex".into());
        assert_ne!(normalized_shared_policy(&saved, &[]), normalized_shared_policy(&changed, &[]));
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
        let session = search.session.clone();
        let _ = search.enqueue(Job { target: Target::Hover(window::Id::unique(), 0), query: "犬".into(),
            tokenize: false, session: session.clone() });
        let _ = update(&mut search, Message::Finished(first.target, Ok(Box::new(Reply {
            result: SearchResult::Found(Box::new(crate::popup::canned())), tokens: Vec::new(),
            session,
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
        let session = search.session.clone();
        let mut definition = Definition::new(
            crate::popup::canned(), session.clone(), search.theme.clone(),
            &mut search.engine, search.media.as_mut(),
        ).unwrap();
        definition.hovered = Some("食べる".into());
        definition.generation = 7;
        search.definitions.push((id, definition));
        let hover = Target::Hover(id, 7);
        let _ = search.enqueue(Job {
            target: hover.clone(), query: "食べる".into(), tokenize: false, session: session.clone(),
        });
        assert_eq!(receiver.recv().unwrap().0.query, "食べる");

        let _ = update(&mut search, Message::Word(2));
        assert!(search.definitions.is_empty());
        assert!(receiver.try_recv().is_err());
        let _ = update(&mut search, Message::Finished(hover, Ok(Box::new(Reply {
            result: SearchResult::Found(Box::new(crate::popup::canned())),
            tokens: Vec::new(), session,
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

    #[test]
    fn escape_from_a_definition_cancels_hover_reopen_work() {
        let (mut search, receiver) = fixture();
        let parent = window::Id::unique();
        let child = window::Id::unique();
        let session = search.session.clone();
        for id in [parent, child] {
            let definition = Definition::new(
                crate::popup::canned(), session.clone(), search.theme.clone(),
                &mut search.engine, search.media.as_mut(),
            ).unwrap();
            search.definitions.push((id, definition));
        }
        search.definitions[0].1.hovered = Some("猫".into());
        search.definitions[0].1.generation = 4;
        let active = Target::Hover(parent, 4);
        let _ = search.enqueue(Job {
            target: active.clone(), query: "猫".into(), tokenize: false, session: session.clone(),
        });
        assert_eq!(receiver.recv().unwrap().0.query, "猫");
        search.pending = Some(Job {
            target: Target::Hover(child, 1), query: "犬".into(), tokenize: false,
            session: session.clone(),
        });

        let _ = update(&mut search, Message::Back(child));
        assert_eq!(search.definitions.len(), 1);
        assert!(search.definitions[0].1.hovered.is_none());
        assert_eq!(search.definitions[0].1.generation, 5);
        assert!(search.pending.is_none());
        let _ = update(&mut search, Message::Finished(active, Ok(Box::new(Reply {
            result: SearchResult::Found(Box::new(crate::popup::canned())),
            tokens: Vec::new(), session,
        }))));
        assert_eq!(search.definitions.len(), 1);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn escape_from_main_invalidates_all_search_work() {
        let (mut search, _) = fixture();
        search.generation = 8;
        search.busy = true;
        search.pending = Some(Job {
            target: Target::Input(8), query: "猫".into(), tokenize: false,
            session: search.session.clone(),
        });
        let main = search.main;
        let _ = update(&mut search, Message::Back(main));
        assert_eq!(search.generation, 9);
        assert!(search.pending.is_none());
        assert!(search.definitions.is_empty());
    }

    #[test]
    fn search_font_roles_and_sentence_scale_match_the_theme() {
        let (mut search, _) = fixture();
        search.theme.headword_weight = 700;
        search.theme.collapsed_italic = true;
        search.theme.body_size = 20.0;
        search.theme.headword_size = 30.0;
        search.theme.collapsed_size = 32.0;
        let headword = themed_font(
            search.font, search.theme.headword_weight, search.theme.headword_italic,
        );
        let summary = themed_font(
            search.font, search.theme.collapsed_weight, search.theme.collapsed_italic,
        );
        assert_eq!(headword.weight, font::Weight::Bold);
        assert_eq!(summary.style, font::Style::Italic);
        assert_eq!(sentence_size(&search.theme), 34.0);
    }
}
