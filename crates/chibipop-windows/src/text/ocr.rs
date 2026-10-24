//! This module provides Windows OCR recognition.

use chibipop::geom::PhysRect;
use chibipop::text::layout::{OcrLine, OcrWord};
use anyhow::{Context, Result};
use std::time::Duration;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Blocks until the operation finishes because `.get()` is unavailable.
fn wait_blocking<T>(op: windows_future::IAsyncOperation<T>) -> Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    let (send, recv) = std::sync::mpsc::sync_channel(1);
    op.SetCompleted(&windows_future::AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = send.try_send(());
        Ok(())
    }))?;
    if recv.recv_timeout(OCR_TIMEOUT).is_err() {
        let _ = op.Cancel();
        anyhow::bail!("OCR did not finish within {OCR_TIMEOUT:?}");
    }
    Ok(op.GetResults()?)
}

/// Sets the limit for one OCR call.
/// If the operation remains pending until `OCR_TIMEOUT`, `wait_blocking` returns an error.
const OCR_TIMEOUT: Duration = Duration::from_secs(5);

/// The coordinates refer to the upscaled image.
pub fn recognise(engine: &OcrEngine, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
    let ibuffer = CryptographicBuffer::CreateFromByteArray(buf)
        .context("wrapping the pixel buffer")?;
    // The buffer uses 32bpp BGRA. The alpha channel has no useful data.
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &ibuffer,
        BitmapPixelFormat::Bgra8,
        w,
        h,
        BitmapAlphaMode::Ignore,
    )
    .context("building a SoftwareBitmap from the capture")?;

    let result = wait_blocking(engine.RecognizeAsync(&bitmap)?)
        .context("running OCR")?;

    let mut lines = Vec::new();
    // Call Size(), not Count(), for this Windows collection.
    for line in &result.Lines()? {
        let mut words = Vec::new();
        for word in &line.Words()? {
            let r = word.BoundingRect()?;
            words.push(OcrWord {
                text: word.Text()?.to_string(),
                rect: PhysRect {
                    x: r.X as i32,
                    y: r.Y as i32,
                    w: r.Width as i32,
                    h: r.Height as i32,
                },
            });
        }
        if !words.is_empty() {
            lines.push(OcrLine { words });
        }
    }
    Ok(lines)
}

fn extends_at_boundary(long: &str, short: &str) -> bool {
    let (long, short) = (long.as_bytes(), short.as_bytes());
    long.len() >= short.len()
        && long[..short.len()].eq_ignore_ascii_case(short)
        && (long.len() == short.len()
            || (long.len() > short.len() + 1 && long[short.len()] == b'-'))
}

/// This function matches tags only at a subtag boundary.
pub fn tag_matches(reported: &str, wanted: &str) -> bool {
    extends_at_boundary(reported, wanted) || extends_at_boundary(wanted, reported)
}

/// Reports whether Windows has an installed recognizer for `tag`.
pub fn recogniser_available(tag: &str) -> bool {
    if !Language::IsWellFormed(&HSTRING::from(tag)).unwrap_or(false) {
        return false;
    }
    let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
        return false;
    };
    langs.into_iter().any(|l| {
        l.LanguageTag()
            .map(|t| tag_matches(&t.to_string(), tag))
            .unwrap_or(false)
    })
}

/// Returns each installed recognizer as a name and tag pair.
///
/// It returns an entry only when its `LanguageTag` query succeeds.
///
/// Returns an empty vector if the Windows call fails.
pub fn installed_recognisers() -> Vec<(String, String)> {
    let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for l in langs {
        let Ok(tag) = l.LanguageTag() else {
            continue;
        };
        let tag = tag.to_string();
        let name = l.DisplayName().map(|n| n.to_string()).unwrap_or_else(|_| tag.clone());
        out.push((name, tag));
    }
    out
}

/// Creates a WinRT OCR engine for a language tag.
fn make_engine(language: &str) -> Result<OcrEngine> {
    let lang = Language::CreateLanguage(&HSTRING::from(language))?;
    OcrEngine::TryCreateFromLanguage(&lang).with_context(|| {
        format!(
            "no OCR recogniser for {language} - add it under Windows Settings, \
             Time & language, Language & region"
        )
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum LangAction {
    Keep,
    Swap,
    NoPack,
}

fn language_action(current: &str, requested: &str, available: impl FnOnce() -> bool)
    -> LangAction {
    if requested.eq_ignore_ascii_case(current) {
        LangAction::Keep
    } else if available() {
        LangAction::Swap
    } else {
        LangAction::NoPack
    }
}

/// This type provides the WinRT OCR backend for core's `OcrEngine` seam.
pub struct WinrtOcr {
    engine: OcrEngine,
    language: String,
    monitor: Option<crate::text::runtime::OcrMonitor>,
}

impl WinrtOcr {
    /// Initializes WinRT and the OCR engine once.
    pub fn new(language: &str) -> Result<Self> {
        // Without this call, later WinRT calls can return `CO_E_NOTINITIALIZED`.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let engine = make_engine(language)?;
        Ok(WinrtOcr { engine, language: language.to_string(), monitor: None })
    }

    /// Returns the engine that `new` created.
    pub fn engine(&self) -> &OcrEngine {
        &self.engine
    }

    pub(crate) fn with_monitor(mut self, monitor: crate::text::runtime::OcrMonitor) -> Self {
        self.monitor = Some(monitor);
        self
    }

    pub(crate) fn active_language(&self) -> String {
        self.engine.RecognizerLanguage().and_then(|language| language.LanguageTag())
            .map_or_else(|_| self.language.clone(), |tag| tag.to_string())
    }
}

impl chibipop::text::OcrEngine for WinrtOcr {
    fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        recognise(&self.engine, bgra, w, h)
    }

    /// Tries to replace the OCR engine with the requested language.
    fn set_language(&mut self, language: &str) {
        match language_action(&self.language, language, || recogniser_available(language)) {
            LangAction::Keep => {}
            LangAction::NoPack => {
                eprintln!("chibipop: no {language} recogniser; keeping {}", self.language);
            }
            LangAction::Swap => match make_engine(language) {
                Ok(built) => {
                    self.engine = built;
                    self.language = language.to_string();
                }
                Err(e) => eprintln!(
                    "chibipop: {language} recogniser failed, keeping {}: {e:#}",
                    self.language
                ),
            },
        }
        if let Some(monitor) = &self.monitor {
            monitor.publish("windows-ocr", &self.active_language(), true);
        }
    }

    /// This is the upstream plugin-parity name for version 0.9.x.
    /// Engine selection and the `probe` treat the built-in engine and a plugin
    /// adapter in the same way.
    fn name(&self) -> &str {
        "windows-ocr"
    }

    fn provides_geometry(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;




    #[test]
    fn a_new_language_with_a_pack_is_swapped_in() {
        assert_eq!(LangAction::Swap, language_action("ja", "ko", || true));
    }

    #[test]
    fn a_new_language_with_no_pack_is_refused() {
        assert_eq!(LangAction::NoPack, language_action("ja", "ko", || false));
    }

    #[test]
    fn refused_language_change_reports_the_engine_language_that_remains_active() {
        let Some(tag) = OcrEngine::AvailableRecognizerLanguages().ok()
            .and_then(|languages| languages.into_iter().next())
            .and_then(|language| language.LanguageTag().ok()) else {
                eprintln!("UNAVAILABLE: no installed OCR recognizer");
                return;
            };
        let monitor = crate::text::runtime::OcrMonitor::default();
        let mut engine = WinrtOcr::new(&tag.to_string()).unwrap().with_monitor(monitor.clone());
        chibipop::text::OcrEngine::set_language(&mut engine, &tag.to_string());
        let before = monitor.snapshot();
        chibipop::text::OcrEngine::set_language(&mut engine, "xx-Fake");
        assert_eq!(before, monitor.snapshot());
        assert_eq!(engine.active_language(), before.language);
        assert!(before.available);
    }

    #[test]
    fn a_more_specific_reported_tag_matches() {
        assert!(tag_matches("zh-Hans-CN", "zh-Hans"));
        assert!(tag_matches("en-US", "en"));
        assert!(tag_matches("zh-Hans-CN", "zh"));
    }
}
