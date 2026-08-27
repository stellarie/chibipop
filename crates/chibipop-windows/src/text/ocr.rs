//! Windows OCR recognition.

use chibipop::geom::PhysRect;
use chibipop::text::layout::{OcrLine, OcrWord};
use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Blocks; .get() is gone.
fn wait_blocking<T>(op: windows_future::IAsyncOperation<T>) -> Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    let deadline = Instant::now() + OCR_TIMEOUT;
    loop {
        if op.Status()? != windows_future::AsyncStatus::Started {
            return Ok(op.GetResults()?);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("OCR did not finish within {OCR_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Bound on one OCR call.
const OCR_TIMEOUT: Duration = Duration::from_secs(5);

/// Coords are upscaled-image.
pub fn recognise(engine: &OcrEngine, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
    let ibuffer = CryptographicBuffer::CreateFromByteArray(buf)
        .context("wrapping the pixel buffer")?;
    // 32bpp BGRA; alpha is junk.
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
    // Size(), not Count().
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

/// Subtag-boundary tag match.
pub fn tag_matches(reported: &str, wanted: &str) -> bool {
    extends_at_boundary(reported, wanted) || extends_at_boundary(wanted, reported)
}

/// Is this recogniser installed?
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

/// Installed ones: name, tag.
///
/// Empty if the call fails.
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

/// Builds an engine for a tag.
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

/// The WinRT OCR backend.
pub struct WinrtOcr {
    engine: OcrEngine,
    language: String,
}

impl WinrtOcr {
    /// Inits WinRT + engine once.
    pub fn new(language: &str) -> Result<Self> {
        // Else CO_E_NOTINITIALIZED.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let engine = make_engine(language)?;
        Ok(WinrtOcr { engine, language: language.to_string() })
    }

    /// The engine from `new`.
    pub fn engine(&self) -> &OcrEngine {
        &self.engine
    }
}

impl chibipop::text::OcrEngine for WinrtOcr {
    fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        recognise(&self.engine, bgra, w, h)
    }

    /// Swap in a new language.
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
    }

    /// Upstream's plugin-parity name (0.9.x): engine selection and the
    /// probe treat the built-in and a plugin adapter uniformly.
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
    fn an_unchanged_language_is_kept() {
        assert_eq!(LangAction::Keep, language_action("ja", "ja", || true));
    }

    /// The guard folds case.
    #[test]
    fn an_unchanged_language_is_kept_even_with_no_pack() {
        assert_eq!(LangAction::Keep, language_action("ja", "JA", || false));
    }

    #[test]
    fn a_new_language_with_a_pack_is_swapped_in() {
        assert_eq!(LangAction::Swap, language_action("ja", "ko", || true));
    }

    #[test]
    fn a_new_language_with_no_pack_is_refused() {
        assert_eq!(LangAction::NoPack, language_action("ja", "ko", || false));
    }

    /// No WinRT call on the no-op.
    #[test]
    fn an_unchanged_language_never_asks_windows() {
        let mut asked = false;
        let got = language_action("ja", "ja", || {
            asked = true;
            true
        });
        assert_eq!(LangAction::Keep, got);
        assert!(!asked);
    }

    #[test]
    fn a_nonsense_tag_is_not_available() {
        assert!(!recogniser_available("xx-Fake"));
    }

    /// True on any machine.
    #[test]
    fn every_installed_recogniser_reports_available() {
        let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
            return;
        };
        for l in langs {
            if let Ok(tag) = l.LanguageTag() {
                assert!(recogniser_available(&tag.to_string()));
                assert!(recogniser_available(&tag.to_string().to_uppercase()));
            }
        }
    }

    /// True on any machine.
    #[test]
    fn every_listed_recogniser_is_named_and_available() {
        for (name, tag) in installed_recognisers() {
            assert!(!name.is_empty(), "{tag} has no display name");
            assert!(!tag.is_empty());
            assert!(recogniser_available(&tag));
        }
    }

    #[test]
    fn the_listed_recognisers_match_the_engine_list() {
        let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
            return;
        };
        assert_eq!(langs.Size().unwrap_or(0) as usize, installed_recognisers().len());
    }

    #[test]
    fn an_identical_tag_matches() {
        assert!(tag_matches("ja", "ja"));
        assert!(tag_matches("zh-Hans-CN", "zh-Hans-CN"));
    }

    #[test]
    fn a_tag_matches_whatever_its_case() {
        assert!(tag_matches("en-US", "EN-us"));
        assert!(tag_matches("ZH-HANS-CN", "zh-hans"));
    }

    #[test]
    fn a_more_specific_reported_tag_matches() {
        assert!(tag_matches("zh-Hans-CN", "zh-Hans"));
        assert!(tag_matches("en-US", "en"));
        assert!(tag_matches("zh-Hans-CN", "zh"));
    }

    #[test]
    fn a_more_specific_wanted_tag_matches() {
        assert!(tag_matches("ja", "ja-JP"));
        assert!(tag_matches("zh", "zh-Hant-TW"));
    }

    #[test]
    fn a_different_script_does_not_match() {
        assert!(!tag_matches("zh-Hant-TW", "zh-Hans"));
        assert!(!tag_matches("zh-Hans-CN", "zh-Hant"));
    }

    /// Boundary, not starts_with.
    #[test]
    fn a_partial_subtag_does_not_match() {
        assert!(!tag_matches("zh-Hans-CN", "zh-Han"));
        assert!(!tag_matches("zh-Han", "zh-Hans-CN"));
        assert!(!tag_matches("jav", "ja"));
        assert!(!tag_matches("ja", "jav"));
    }

    #[test]
    fn an_unrelated_tag_does_not_match() {
        assert!(!tag_matches("ja", "ko"));
        assert!(!tag_matches("en-US", "ja"));
    }

    #[test]
    fn an_empty_tag_matches_nothing_real() {
        assert!(!tag_matches("ja", ""));
        assert!(!tag_matches("", "ja"));
    }

    #[test]
    fn matching_is_symmetric() {
        let tags = ["ja", "ja-JP", "en", "en-US", "zh", "zh-Hans", "zh-Hans-CN", "zh-Hant-TW"];
        for a in tags {
            for b in tags {
                assert_eq!(tag_matches(a, b), tag_matches(b, a), "{a} vs {b}");
            }
        }
    }

    #[test]
    fn a_trailing_hyphen_does_not_match() {
        assert!(!tag_matches("ja", "ja-"));
        assert!(!tag_matches("ja-", "ja"));
        assert!(!tag_matches("JA", "ja-"));
        assert!(!tag_matches("zh-Hans", "zh-Hans-"));
        assert!(!tag_matches("zh-Hans-", "zh-Hans"));
    }

    #[test]
    fn a_lone_hyphen_matches_nothing() {
        assert!(!tag_matches("-", ""));
        assert!(!tag_matches("", "-"));
    }

    #[test]
    fn a_leading_hyphen_does_not_match() {
        assert!(!tag_matches("ja", "-ja"));
        assert!(!tag_matches("-ja", "ja"));
        assert!(!tag_matches("en-US", "-US"));
    }

    /// True on any machine.
    #[test]
    fn a_malformed_tag_is_never_available() {
        for t in ["ja-", "ja--JP", "ja--", "ja---", "JA--jp", "-ja", "-", "", "ja-JP-"] {
            assert!(!recogniser_available(t), "{t:?}");
        }
    }
}
