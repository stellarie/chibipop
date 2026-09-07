//! Reports concrete OCR state without extending the core OCR seam.

use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OcrRuntime {
    pub engine: String,
    pub language: String,
    pub available: bool,
}

#[derive(Clone, Default)]
pub struct OcrMonitor(Arc<Mutex<OcrRuntime>>);

impl OcrMonitor {
    pub fn snapshot(&self) -> OcrRuntime {
        self.0.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn publish(&self, engine: &str, language: &str, available: bool) {
        let mut current = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if current.engine != engine || current.language != language || current.available != available {
            *current = OcrRuntime { engine: engine.into(), language: language.into(), available };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_shares_actual_backend_state_without_changing_configuration() {
        let monitor = OcrMonitor::default();
        let backend = monitor.clone();
        assert!(!monitor.snapshot().available);
        backend.publish("windows-ocr", "ja", true);
        assert_eq!("ja", monitor.snapshot().language);
        backend.publish("meikiocr", "zh-Hans", true);
        assert_eq!("meikiocr", monitor.snapshot().engine);
        backend.publish("meikiocr", "zh-Hans", false);
        assert!(!monitor.snapshot().available);
        assert_eq!("zh-Hans", monitor.snapshot().language);
    }
}
