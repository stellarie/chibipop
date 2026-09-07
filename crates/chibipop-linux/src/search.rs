//! Search runs separately so its input focus and database lifetime do not
//! interfere with the daemon. Each submit reloads configuration and SQLite.

use crate::paths::Paths;
use anyhow::{Context, Result};
use chibipop::search::{result_text, SearchResult, SearchService};
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Task};
use std::path::PathBuf;
use std::process::Command;

fn focus_path(paths: &Paths) -> Result<PathBuf> {
    let display = crate::wayland::display_name()?;
    Ok(paths.runtime_dir()?.join(format!("search-focus-{}.lock", crate::lock::sanitize(&display))))
}

/// Probe a live lock rather than a PID record, which can survive a crash.
/// Missing files mean no search window has published focus in this session.
pub fn is_focused(paths: &Paths) -> Result<bool> {
    focus::is_held(&focus_path(paths)?).context("probing dictionary search focus")
}

pub fn search_command(paths: &Paths) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--config").arg(&paths.config_file).arg("search");
    crate::signals::unmasked(&mut command);
    Ok(command)
}

pub fn run(paths: Paths) -> Result<()> {
    let display = crate::wayland::display_name()?;
    let name = format!("search-{}.lock", crate::lock::sanitize(&display));
    let _lock = match crate::lock::acquire_at(paths.runtime_dir()?, &name) {
        Ok(lock) => lock,
        Err(crate::lock::LockError::AlreadyRunning { .. }) => return Ok(()),
        Err(crate::lock::LockError::Io(error)) => return Err(error.into()),
    };
    let focus_path = focus_path(&paths)?;
    focus::prepare(&focus_path).context("preparing dictionary search focus")?;
    iced::application(move || (Search {
        paths: paths.clone(), query: String::new(), busy: false,
        result: result_text(&SearchResult::Empty),
        focus_path: focus_path.clone(), focus: None,
    }, iced::widget::operation::focus("search-query")), update, view)
        .title("chibipop search")
        .window_size((760.0, 640.0))
        .subscription(|_| iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focused(true)),
            iced::Event::Window(iced::window::Event::Unfocused)
            | iced::Event::Window(iced::window::Event::Closed) => Some(Message::Focused(false)),
            _ => None,
        }))
        .run().context("running dictionary search")
}

struct Search {
    paths: Paths, query: String, result: String, busy: bool,
    focus_path: PathBuf, focus: Option<focus::Lease>,
}

#[derive(Debug, Clone)]
enum Message { Input(String), Submit, Finished(String), Focused(bool) }

fn update(search: &mut Search, message: Message) -> Task<Message> {
    match message {
        Message::Focused(false) => { search.focus = None; }
        Message::Focused(true) if search.focus.is_none() => {
            match focus::Lease::acquire(&search.focus_path) {
                Ok(lease) => search.focus = Some(lease),
                Err(error) => search.result = format!("Cannot protect search input focus: {error}"),
            }
        }
        Message::Focused(true) => {}
        Message::Input(query) => search.query = query,
        Message::Finished(result) => { search.busy = false; search.result = result; }
        Message::Submit if !search.busy => {
            if search.query.trim().is_empty() {
                search.result = result_text(&SearchResult::Empty);
                return Task::none();
            }
            search.busy = true;
            search.result = "Searching…".into();
            let paths = search.paths.clone();
            let query = search.query.clone();
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            if let Err(error) = std::thread::Builder::new().name("dictionary-search".into()).spawn(move || {
                let result = query_database(&paths, &query).unwrap_or_else(|error|
                    format!("Search failed: {error:#}\nCheck the dictionaries in Settings, then search again."));
                let _ = tx.send(result);
            }) {
                search.busy = false;
                search.result = format!("Cannot start search: {error}");
                return Task::none();
            }
            return Task::perform(rx, |result| Message::Finished(result.unwrap_or_else(|_|
                "Search stopped unexpectedly. Submit again to retry.".into())));
        }
        Message::Submit => {}
    }
    Task::none()
}

fn view(search: &Search) -> Element<'_, Message> {
    container(column![
        row![text_input("Japanese word or expression", &search.query)
            .id("search-query").on_input(Message::Input).on_submit(Message::Submit).padding(10),
            button("Search").on_press_maybe((!search.busy).then_some(Message::Submit))].spacing(10),
        scrollable(text(&search.result).size(18)).height(Length::Fill),
    ].spacing(16)).padding(16).into()
}

fn query_database(paths: &Paths, query: &str) -> Result<String> {
    let config = chibipop::config::load_or_create(&paths.config_file)?;
    let service = SearchService::open(&paths.data_dir.join("chibipop.sqlite"), &rules_file(), &config)?;
    Ok(result_text(&service.search(query)?))
}

fn rules_file() -> PathBuf {
    let rules = "data/deconjugator.json";
    let beside = chibipop::paths::beside_exe(rules);
    if beside.is_file() { return beside; }
    let shared = chibipop::paths::beside_exe(&format!("../share/chibipop/{rules}"));
    if shared.is_file() { return shared; }
    chibipop::paths::data_file(rules)
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

    fn paths() -> Paths {
        crate::paths::resolve(&crate::paths::Env::from_process(), Some("/tmp/search config.toml".into()))
    }

    #[test]
    fn child_command_preserves_config_override() {
        let command = search_command(&paths()).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--config", "/tmp/search config.toml", "search"]);
    }

    #[test]
    fn japanese_input_empty_submit_and_reply_preserve_query() {
        let mut search = Search { paths: paths(), query: String::new(), result: String::new(), busy: false,
            focus_path: PathBuf::new(), focus: None };
        let _ = update(&mut search, Message::Submit);
        assert_eq!(search.result, result_text(&SearchResult::Empty));
        let _ = update(&mut search, Message::Input("食べました".into()));
        search.busy = true;
        let _ = update(&mut search, Message::Finished("to eat".into()));
        assert_eq!(search.query, "食べました");
        assert_eq!(search.result, "to eat");
        assert!(!search.busy);
    }

    #[test]
    fn focus_messages_are_idempotent_and_drop_releases_focus() {
        let path = std::env::temp_dir().join(format!("chibipop-search-focus-events-{}.lock", std::process::id()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
        }
        let _cleanup = Cleanup(path.clone());
        focus::prepare(&path).unwrap();
        let mut search = Search { paths: paths(), query: String::new(), result: String::new(), busy: false,
            focus_path: path.clone(), focus: None };
        assert!(!focus::is_held(&path).unwrap());
        let _ = update(&mut search, Message::Focused(true));
        let _ = update(&mut search, Message::Focused(true));
        assert!(focus::is_held(&path).unwrap());
        let _ = update(&mut search, Message::Focused(false));
        assert!(!focus::is_held(&path).unwrap());
        let _ = update(&mut search, Message::Focused(true));
        assert!(focus::is_held(&path).unwrap());
        drop(search);
        assert!(!focus::is_held(&path).unwrap());
    }
}
