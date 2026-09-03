use super::{fallback_words, Analyzer, TextKey, WordMap};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

struct Request {
    generation: u64,
    texts: Vec<(TextKey, String)>,
}

/// A handle for the one-thread Japanese analysis service.
///
/// The service owns the model after its first request. It sends one result for
/// each request that it accepts and never joins its thread during process exit.
pub struct Service {
    request_tx: mpsc::Sender<Request>,
    result_rx: mpsc::Receiver<(u64, WordMap)>,
    fallback_active: Arc<AtomicBool>,
}

impl Service {
    /// Start the service with a model path and an event-loop wake callback.
    ///
    /// The model is read only after the first request. The callback runs after
    /// each result reaches the result channel.
    pub fn spawn(model: PathBuf, wake: impl Fn() + Send + 'static) -> Service {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<(u64, WordMap)>();
        let fallback_active = Arc::new(AtomicBool::new(false));
        let thread_fallback_active = Arc::clone(&fallback_active);

        // Do not join this thread. The process can exit while it waits for a request.
        thread::spawn(move || {
            run(
                model,
                wake,
                request_rx,
                result_tx,
                thread_fallback_active,
            )
        });

        Service { request_tx, result_rx, fallback_active }
    }

    /// Queue the newest analysis request.
    pub fn request(&self, generation: u64, texts: Vec<(TextKey, String)>) {
        let _ = self.request_tx.send(Request { generation, texts });
    }

    /// Return the result receiver. The platform bin drains it after the wake callback.
    pub fn results(&self) -> &mpsc::Receiver<(u64, WordMap)> {
        &self.result_rx
    }

    /// Return whether the service emitted its one load-failure diagnostic.
    ///
    /// When this is true, every request uses [`fallback_words`](super::fallback_words).
    /// This state lets callers and tests observe the one-time diagnostic without
    /// replacing stderr with a second logging interface.
    pub fn fallback_active(&self) -> bool {
        self.fallback_active.load(Ordering::Acquire)
    }
}

fn run(
    model: PathBuf,
    wake: impl Fn(),
    request_rx: mpsc::Receiver<Request>,
    result_tx: mpsc::Sender<(u64, WordMap)>,
    fallback_active: Arc<AtomicBool>,
) {
    let mut analyzer = None;
    let mut fallback = false;

    while let Ok(first) = request_rx.recv() {
        let mut request = first;
        while let Ok(newest) = request_rx.try_recv() {
            request = newest;
        }

        if analyzer.is_none() && !fallback {
            match Analyzer::load(&model) {
                Ok(loaded) => analyzer = Some(loaded),
                Err(error) => {
                    eprintln!("chibipop: japanese analysis unavailable: {error:#}");
                    fallback = true;
                    fallback_active.store(true, Ordering::Release);
                }
            }
        }

        let mut words = HashMap::with_capacity(request.texts.len());
        for (key, text) in request.texts {
            let ranges = match analyzer.as_mut() {
                Some(analyzer) => analyzer.analyze(&text).words,
                None => fallback_words(&text),
            };
            words.insert(key, ranges);
        }

        if result_tx.send((request.generation, words)).is_err() {
            break;
        }
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::gloss::NodePath;
    use std::time::Duration;

    #[test]
    fn a_failed_load_uses_fallback_and_reports_the_failure_once() {
        let model = std::env::temp_dir().join(format!(
            "chibipop-analysis-missing-{}",
            std::process::id()
        ));
        let service = Service::spawn(model, || {});
        let key = (0, NodePath::ROOT.child(0).unwrap());

        service.request(7, vec![(key, "Hello world".to_string())]);
        let (generation, result) = service
            .results()
            .recv_timeout(Duration::from_secs(5))
            .expect("the failed load must still produce a result");
        assert_eq!(7, generation);
        assert_eq!(vec![0..5, 6..11], result.get(&key).unwrap().clone());
        assert!(service.fallback_active(), "the one load diagnostic must be observable");

        service.request(8, vec![(key, "again".to_string())]);
        let (generation, result) = service
            .results()
            .recv_timeout(Duration::from_secs(5))
            .expect("fallback must answer later requests");
        assert_eq!(8, generation);
        assert_eq!(vec![0..5], result.get(&key).unwrap().clone());
        assert!(service.fallback_active(), "the diagnostic state must remain active");
    }
}
