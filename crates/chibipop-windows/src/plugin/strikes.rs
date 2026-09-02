//! Track consecutive plugin failures so the host can disable a plugin at its configured limit.

pub struct Strikes {
    limit: u8,
    count: u8,
    disabled: bool,
    last: Option<String>,
}

impl Strikes {
    pub fn new(limit: u8) -> Self {
        Strikes { limit, count: 0, disabled: false, last: None }
    }

    pub fn note(&mut self, e: &str) {
        self.last = Some(e.to_string());
    }

    pub fn record(&mut self, ok: bool) -> Option<String> {
        if ok {
            self.count = 0;
            return None;
        }
        if self.disabled {
            return None;
        }
        self.count += 1;
        if self.count < self.limit {
            return None;
        }
        self.disabled = true;
        let why = self.last.as_deref().unwrap_or("no detail recorded");
        Some(format!("plugin disabled after {} failures: {why}", self.limit))
    }

    pub fn disabled(&self) -> bool {
        self.disabled
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_failures_do_not_disable() {
        let mut s = Strikes::new(3);
        assert!(s.record(false).is_none());
        assert!(s.record(false).is_none());
        assert!(!s.disabled());
    }

    #[test]
    fn the_third_consecutive_failure_disables_and_names_the_error() {
        let mut s = Strikes::new(3);
        s.note("missed its 500 ms deadline");
        s.record(false);
        s.record(false);
        let msg = s.record(false).expect("a notice");
        assert!(s.disabled());
        assert!(msg.contains("500 ms"), "{msg}");
    }

    #[test]
    fn a_success_resets_the_count() {
        let mut s = Strikes::new(3);
        s.record(false);
        s.record(false);
        s.record(true);
        assert!(s.record(false).is_none());
        assert!(!s.disabled());
    }

    #[test]
    fn the_notice_fires_exactly_once() {
        let mut s = Strikes::new(2);
        s.record(false);
        assert!(s.record(false).is_some());
        assert!(s.record(false).is_none());
    }
}
