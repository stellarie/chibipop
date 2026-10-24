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
        database.clone(), saved, identity.profile_id.clone(),
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
            generation: 0, click_generation: 0, worker: worker.clone(), busy: false, pending: None,
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
    click_generation: u64,
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
enum Target { Input(u64), Word(u64), Hover(window::Id, u64), Click(window::Id, u64) }

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
    fn new(database: PathBuf, saved: Config, profile_id: String)
        -> Result<(Self, ProfileSession)> {
        let (sender, receiver) = mpsc::channel::<(Job, WorkerReply)>();
        let (startup_sender, startup_receiver) = mpsc::channel::<Result<ProfileSession>>();
        thread::Builder::new().name("search-worker".into()).spawn(move || {
            let (service, session) = match SearchService::open_catalog(
                &database, None, &saved, &profile_id,
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
            let explicit_pending = self.pending.as_ref().is_some_and(|pending|
                matches!(pending.target, Target::Input(_) | Target::Word(_) | Target::Click(_, _)));
            if !explicit_pending || !matches!(job.target, Target::Hover(_, _)) { self.pending = Some(job); }
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

    fn enqueue_click(&mut self, id: window::Id, query: String) -> Task<Message> {
        let session = {
            let Some((_, definition)) = self.definitions.iter_mut().find(|(known, _)| *known == id) else {
                return Task::none();
            };
            definition.hovered = Some(query.clone());
            definition.generation = definition.generation.wrapping_add(1);
            definition.session.nested()
        };
        self.cancel_pending_hover(id);
        self.cancel_clicks();
        let target = Target::Click(id, self.click_generation);
        self.enqueue(Job { target, query, tokenize: false, session })
    }

    fn input_changed(&mut self) -> Task<Message> {
        self.generation = self.generation.wrapping_add(1);
        self.cancel_clicks();
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
        if self.pending.as_ref().is_some_and(|job| {
            let owner = match &job.target {
                Target::Hover(id, _) | Target::Click(id, _) => Some(*id),
                _ => None,
            };
            owner.is_some_and(|id| self.definitions[depth..].iter().any(|(retired, _)| *retired == id))
        }) {
            self.pending = None;
        }
        let mut tasks = Vec::new();
        for (id, _) in self.definitions.drain(depth..) {
            self.focused.remove(&id);
            self.hovered.remove(&id);
            tasks.push(window::close(id));
        }
        self.protect();
        Task::batch(tasks)
    }

    fn cancel_pending_hover(&mut self, id: window::Id) {
        if self.pending.as_ref().is_some_and(|job|
            matches!(&job.target, Target::Hover(target, _) if *target == id)) {
            self.pending = None;
        }
    }

    fn cancel_pending_hovers(&mut self) {
        if self.pending.as_ref().is_some_and(|job| matches!(job.target, Target::Hover(_, _))) {
            self.pending = None;
        }
    }

    fn cancel_clicks(&mut self) {
        self.click_generation = self.click_generation.wrapping_add(1);
        if self.pending.as_ref().is_some_and(|job| matches!(job.target, Target::Click(_, _))) {
            self.pending = None;
        }
    }

    fn cancel_hovers(&mut self) {
        self.cancel_pending_hovers();
        for (_, definition) in &mut self.definitions {
            definition.hovered = None;
            definition.generation = definition.generation.wrapping_add(1);
        }
    }

    fn cancel_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.cancel_hovers();
        self.cancel_clicks();
    }

    fn open_definition(&mut self, presentation: chibipop::present::Presentation,
        session: ProfileSession) -> Task<Message> {
        if self.definitions.len() >= MAX_DEFINITIONS {
            self.status = "Close a definition window before opening another level.".into();
            return Task::none();
        }
        self.cancel_clicks();
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
            search.cancel_clicks();
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
                Target::Click(id, generation) => search.click_generation == generation
                    && search.definitions.iter().any(|(known, _)| *known == id),
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
                            Target::Hover(id, _) | Target::Click(id, _) => {
                                if let Some(presentation) = selected_presentation(&result, 0) {
                                    if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                                        let parent = &search.definitions[depth].1;
                                        let same_word = parent.session == session
                                            && presentation.top.as_ref().zip(parent.presentation.top.as_ref())
                                                .is_some_and(|(a, b)| a.written == b.written && a.reading == b.reading);
                                        if !same_word {
                                            if matches!(target, Target::Click(_, _)) {
                                                search.cancel_hovers();
                                            }
                                            tasks.push(search.close_from(depth + 1));
                                            tasks.push(search.open_definition(presentation, session));
                                        }
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
            let (query, reentered, unchanged) = {
                let definition = &mut search.definitions[depth].1;
                let reentered = definition.pointer_left || definition.pointer != point;
                definition.pointer = point;
                definition.pointer_left = false;
                let query = if definition.session.config().popup.sub_popups {
                    definition.hover(point, &mut search.engine)
                } else { None };
                let unchanged = query.as_ref() == definition.hovered.as_ref();
                (query, reentered, unchanged)
            };
            if query.is_none() && reentered && search.definitions.len() > depth + 1 {
                let definition = &mut search.definitions[depth].1;
                definition.hovered = None;
                definition.generation = definition.generation.wrapping_add(1);
                search.cancel_pending_hovers();
                return search.close_from(depth + 1);
            }
            if unchanged { return Task::none(); }
            search.cancel_pending_hover(id);
            let definition = &mut search.definitions[depth].1;
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
                definition.pointer_left = true;
                definition.hovered = None;
                definition.generation = definition.generation.wrapping_add(1);
            }
            search.cancel_pending_hover(id);
        }
        Message::Back(id) => {
            if let Some(depth) = search.definitions.iter().position(|(known, _)| *known == id) {
                let parent = depth.checked_sub(1).map_or(search.main, |index| search.definitions[index].0);
                search.cancel_hovers();
                search.cancel_clicks();
                return Task::batch([search.close_from(depth), window::gain_focus(parent)]);
            }
            if id == search.main {
                search.cancel_all();
                return Task::batch([search.close_from(0), iced::exit()]);
            }
        }
        Message::Click(id) => {
            let action = search.definitions.iter().find(|(known, _)| *known == id)
                .and_then(|(_, definition)| definition.click());
            match action {
                Some(chibipop::controller::HitAction::Back) => return update(search, Message::Back(id)),
                Some(chibipop::controller::HitAction::DrillDown(query)) => {
                    return search.enqueue_click(id, query);
                }
                _ => {}
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
}
