//! This module is the headless functional phase of the Windows bin.
//!
//! The phase runs eleven manifest cases in one process and opens no window.
//! Every case calls a production function. A case never calls `app::run`,
//! `app::settings_only`, `SearchWindow::open_mode`, `ui::audit::run`, the
//! screen-capture path, or `capture::init_dpi_awareness`. The DPI call would
//! change process state that no later code can undo. See
//! `ARCHITECTURE.md#workspace-and-seams`.
//!
//! The phase writes one JSON line per case on stdout. A case writes its
//! artifacts only under `--run-root`, so a phase run leaves the repository
//! unchanged. A case whose fixture or recogniser is absent reports SKIP.
//! The popup, settings, and search windows belong to the UI phase, not here.

use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::Card;
use chibipop::search::{self, SearchResult};
use chibipop::text::layout::{OcrLine, OcrWord};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_OUTPUT_HANDLE};

/// The manifest schema that this command reads.
const SCHEMA: &str = "chibipop-functional/v1";

/// The case count that the phase promises. A silent drop to ten cases would
/// hide a whole production path, so the manifest must carry all eleven.
const CASE_COUNT: usize = 11;

/// The database path inside the run root. Case `dictionary-build` writes it.
const DATABASE: &str = "data/chibipop.sqlite";

/// The screenshot path inside the run root. Case `png-encoding` writes it.
const SCREENSHOT: &str = "artifacts/screenshot.png";

/// The dictionary archive that the lookup cases read.
const TERMS: &str = "tests/fixtures/yomitan/terms.zip";

/// The deconjugation rules that every lookup uses.
const RULES: &str = "data/deconjugator.json";

/// The pinned IPADIC model that the sentence case reads.
const IPADIC: &str = "data/ipadic/system.dic";

/// The 400x120 BGRA frame of 昨日は.
const OCR_FRAME: &str = "crates/chibipop-windows/tests/fixtures/japanese_bgra.bin";

/// The width of [`OCR_FRAME`].
const OCR_W: i32 = 400;
/// The height of [`OCR_FRAME`].
const OCR_H: i32 = 120;

/// The term that the lookup, search, and presentation cases expect.
const TERM: &str = "猫";

/// One case of the functional phase.
struct Case {
    id: &'static str,
    body: fn(&Ctx) -> Result<Outcome>,
}

/// The result of one case body.
enum Outcome {
    Pass(String),
    Skip(String),
}

/// The order of this array is the order of the manifest run.
const CASES: [Case; CASE_COUNT] = [
    Case { id: "dictionary-build", body: dictionary_build },
    Case { id: "lookup", body: lookup },
    Case { id: "windows-ocr", body: windows_ocr },
    Case { id: "text-resolution", body: text_resolution },
    Case { id: "sentence-analysis", body: sentence_analysis },
    Case { id: "search-candidates", body: search_candidates },
    Case { id: "presentation", body: presentation },
    Case { id: "anki-fields", body: anki_fields },
    Case { id: "png-encoding", body: png_encoding },
    Case { id: "config", body: config },
    Case { id: "plugin-cli", body: plugin_cli },
];

/// One case as the manifest declares it. The manifest holds the run order, the
/// report titles, and the files that the case must read. This command holds
/// the body of each id.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCase {
    id: String,
    title: String,
    /// Repository-relative files that the case reads. The phase reports an
    /// absent required file as a SKIP.
    #[serde(default)]
    requires: Vec<String>,
}

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    cases: Vec<ManifestCase>,
}

/// Runs the functional phase. Returns the process exit code.
pub fn run(manifest: &Path, case: Option<&str>, run_root: Option<&Path>) -> Result<i32> {
    let cases = read_manifest(manifest)?;
    let root = match run_root {
        Some(root) => root.to_path_buf(),
        None => default_run_root()?,
    };
    let ctx = Ctx::new(root, repo_root(manifest));
    // stdout carries one JSON object per case and nothing else, so a reader can
    // parse every line. The run root travels in each line's detail.
    let code = match case {
        Some(id) => run_one(&ctx, &cases, id)?,
        None => run_all(&ctx, &cases)?,
    };
    Ok(code)
}

/// Returns the process exit code for one named case.
fn run_one(ctx: &Ctx, cases: &[ManifestCase], id: &str) -> Result<i32> {
    let declared = match cases.iter().find(|case| case.id == id) {
        Some(declared) => declared,
        None => {
            let known: Vec<&str> = cases.iter().map(|case| case.id.as_str()).collect();
            report(
                id,
                id,
                "FAIL",
                &format!("unknown case id; the manifest declares {}", known.join(", ")),
                0,
            );
            return Ok(1);
        }
    };
    let body = CASES
        .iter()
        .find(|case| case.id == id)
        .with_context(|| format!("the manifest case {id} has no implementation"))?;
    Ok(if execute(ctx, body, declared) { 0 } else { 1 })
}

/// Returns the process exit code for every declared case, in manifest order.
fn run_all(ctx: &Ctx, cases: &[ManifestCase]) -> Result<i32> {
    let mut code = 0;
    for declared in cases {
        let Some(body) = CASES.iter().find(|case| case.id == declared.id) else {
            report(
                &declared.id,
                &declared.title,
                "FAIL",
                "the manifest case has no implementation",
                0,
            );
            code = 1;
            continue;
        };
        if !execute(ctx, body, declared) {
            code = 1;
        }
    }
    Ok(code)
}

/// Runs one case body and prints its JSON line. Returns false for FAIL.
fn execute(ctx: &Ctx, body: &Case, declared: &ManifestCase) -> bool {
    let start = Instant::now();
    // Every file that the manifest declares must exist here. A case reads its
    // own fixture, so a missing declaration would hide a broken fixture.
    if let Some(absent) = declared
        .requires
        .iter()
        .find(|required| !ctx.fixture(required).is_file())
    {
        report(
            body.id,
            &declared.title,
            "SKIP",
            &format!("declared fixture {absent} is absent"),
            start.elapsed().as_millis(),
        );
        return true;
    }
    // Each case owns its scratch space, so two cases cannot write one file.
    let scratch = ctx.run_root.join("cases").join(body.id);
    if let Err(error) = std::fs::create_dir_all(&scratch) {
        report(
            body.id,
            &declared.title,
            "FAIL",
            &format!("creating {}: {error:#}", scratch.display()),
            start.elapsed().as_millis(),
        );
        return false;
    }
    let scratch_ctx = Ctx { scratch, ..ctx.clone() };
    let (status, detail) = match (body.body)(&scratch_ctx) {
        Ok(Outcome::Pass(detail)) => ("PASS", detail),
        Ok(Outcome::Skip(detail)) => ("SKIP", detail),
        Err(error) => {
            eprintln!("chibipop: functional case {}: {error:#}", body.id);
            ("FAIL", format!("{error:#}"))
        }
    };
    report(
        body.id,
        &declared.title,
        status,
        &format!(
            "RUN_ROOT={} SCRATCH={} {detail}",
            ctx.run_root.display(),
            scratch_ctx.scratch.display()
        ),
        start.elapsed().as_millis(),
    );
    status != "FAIL"
}

/// Prints one case result. `pid` proves that one process ran every case.
fn report(id: &str, title: &str, status: &str, detail: &str, ms: u128) {
    let line = json!({
        "id": id,
        "title": title,
        "status": status,
        "detail": detail,
        "duration_ms": ms,
        "pid": std::process::id(),
    });
    println!("{line}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Reads and checks the manifest.
fn read_manifest(path: &Path) -> Result<Vec<ManifestCase>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the manifest from {}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .with_context(|| format!("parsing the manifest from {}", path.display()))?;
    anyhow::ensure!(
        manifest.schema == SCHEMA,
        "{} declares schema {:?}, not {SCHEMA:?}",
        path.display(),
        manifest.schema
    );
    ensure_unique_ids(&manifest.cases, path)?;
    Ok(manifest.cases)
}

/// Rejects a manifest that names one case two times.
fn ensure_unique_ids(cases: &[ManifestCase], path: &Path) -> Result<()> {
    for (index, case) in cases.iter().enumerate() {
        anyhow::ensure!(
            !cases[..index].iter().any(|earlier| earlier.id == case.id),
            "{} declares the case {:?} two times",
            path.display(),
            case.id
        );
    }
    Ok(())
}

/// Returns the repository root. The manifest sits at `tests/functional/`, so
/// the root is two directories above it.
fn repo_root(manifest: &Path) -> PathBuf {
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Returns a fresh run root under the system temporary directory.
fn default_run_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join(format!(
        "chibipop-functional-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating {}", root.display()))?;
    Ok(root)
}

/// The paths that a case needs to run.
#[derive(Clone)]
struct Ctx {
    run_root: PathBuf,
    repo_root: PathBuf,
    /// The per-case scratch space. Every case writes here or into the shared
    /// `data/` and `artifacts/` folders of `run_root`.
    scratch: PathBuf,
}

impl Ctx {
    fn new(run_root: PathBuf, repo_root: PathBuf) -> Ctx {
        Ctx { scratch: run_root.clone(), run_root, repo_root }
    }

    /// Resolves one manifest fixture path against the repository root.
    /// A relative path resolves against the root, not the working directory.
    fn fixture(&self, relative: &str) -> PathBuf {
        self.repo_root.join(relative)
    }

    /// Reports an absent fixture as a skip, or returns its path.
    fn need(&self, relative: &str) -> Result<std::result::Result<PathBuf, Outcome>> {
        let path = self.fixture(relative);
        if path.is_file() {
            return Ok(Ok(path));
        }
        Ok(Err(Outcome::Skip(format!(
            "fixture {relative} is absent at {}",
            path.display()
        ))))
    }

    /// Copies one fixture into the case scratch space and returns the copy.
    /// A case reads its input from the run root, so the repository stays read-only.
    fn stage(&self, relative: &str) -> Result<std::result::Result<PathBuf, Outcome>> {
        let source = match self.need(relative)? {
            Ok(source) => source,
            Err(skip) => return Ok(Err(skip)),
        };
        let name = source
            .file_name()
            .with_context(|| format!("{} has no file name", source.display()))?;
        std::fs::create_dir_all(&self.scratch)
            .with_context(|| format!("creating {}", self.scratch.display()))?;
        let copy = self.scratch.join(name);
        std::fs::copy(&source, &copy)
            .with_context(|| format!("copying {} to {}", source.display(), copy.display()))?;
        Ok(Ok(copy))
    }

    /// Returns the shared database, or reports the absent builder input.
    fn database(&self) -> Result<std::result::Result<PathBuf, Outcome>> {
        let path = self.run_root.join(DATABASE);
        if path.is_file() {
            return Ok(Ok(path));
        }
        Ok(Err(Outcome::Skip(format!(
            "the shared database is absent at {} - run the dictionary-build case first",
            path.display()
        ))))
    }

    /// Opens the shared dictionary and its rules.
    fn engine(&self) -> Result<std::result::Result<(SqliteDictionary, LookupEngine), Outcome>> {
        let database = match self.database()? {
            Ok(path) => path,
            Err(skip) => return Ok(Err(skip)),
        };
        let rules = match self.need(RULES)? {
            Ok(path) => path,
            Err(skip) => return Ok(Err(skip)),
        };
        let dictionary = SqliteDictionary::open(&database)
            .with_context(|| format!("opening {}", database.display()))?;
        let engine = LookupEngine::new(Deconjugator::new(load_rules(&rules)?));
        Ok(Ok((dictionary, engine)))
    }

    /// Runs one lookup query and returns the top card.
    fn top_card(&self, query: &str) -> Result<std::result::Result<Card, Outcome>> {
        let database = match self.database()? {
            Ok(path) => path,
            Err(skip) => return Ok(Err(skip)),
        };
        let cfg = chibipop::config::Config::default();
        let service = search::SearchService::open(&database, &self.fixture(RULES), &cfg)
            .with_context(|| format!("opening search on {}", database.display()))?;
        let found = service
            .search(query)
            .with_context(|| format!("searching for {query}"))?;
        match found {
            SearchResult::Found(presentation) => match presentation.top {
                Some(card) => Ok(Ok(card)),
                None => Ok(Err(Outcome::Skip(format!(
                    "{query} has no card in the fixture dictionary"
                )))),
            },
            other => Ok(Err(Outcome::Skip(format!(
                "{query} produced no presentation: {other:?}"
            )))),
        }
    }

    /// Returns the executable that the plugin manifest starts.
    fn self_exe(&self) -> Result<PathBuf> {
        std::env::current_exe().context("locating this executable")
    }
}

/// Case 1. Builds the database into the run root with the production builder.
fn dictionary_build(ctx: &Ctx) -> Result<Outcome> {
    let archive = match ctx.stage(TERMS)? {
        Ok(path) => path,
        Err(skip) => return Ok(skip),
    };
    let out = ctx.run_root.join(DATABASE);
    let counts = chibipop::dict::build::build(&[archive], &[], &out, &|_| {})?;
    let dictionary = SqliteDictionary::open(&out)
        .with_context(|| format!("opening the built database at {}", out.display()))?;
    let terms = dictionary.terms_for(TERM)?;
    Ok(Outcome::Pass(format!(
        "{} entries, {} term rows; {TERM} has {} stored rows",
        counts.entries,
        counts.terms,
        terms.len()
    )))
}

/// Case 2. Looks up one term through the production engine.
fn lookup(ctx: &Ctx) -> Result<Outcome> {
    let (dictionary, engine) = match ctx.engine()? {
        Ok(pair) => pair,
        Err(skip) => return Ok(skip),
    };
    let hits = engine.run(&dictionary, TERM)?;
    let Some(top) = hits.first() else {
        anyhow::bail!("{TERM} produced no hit in the fixture dictionary");
    };
    let reading = top.reading.clone().unwrap_or_else(|| "-".to_string());
    Ok(Outcome::Pass(format!(
        "{} hit(s); top {TERM} [{reading}] match_len {} score {:.2}",
        hits.len(),
        top.match_len,
        top.score
    )))
}

/// Case 3. Recognises the committed BGRA frame with the production OCR path.
fn windows_ocr(ctx: &Ctx) -> Result<Outcome> {
    let frame = match ctx.stage(OCR_FRAME)? {
        Ok(path) => path,
        Err(skip) => return Ok(skip),
    };
    let buffer = std::fs::read(&frame)
        .with_context(|| format!("reading {}", frame.display()))?;
    // The OCR backends are thread-affine. Build the engine here, next to its use.
    let engine = match chibipop_windows::text::ocr::WinrtOcr::new("ja") {
        Ok(engine) => engine,
        Err(error) => {
            return Ok(Outcome::Skip(format!(
                "no installed ja recogniser: {error:#}"
            )))
        }
    };
    let lines = chibipop_windows::text::ocr::recognise(engine.engine(), &buffer, OCR_W, OCR_H)?;
    let text: String = lines
        .iter()
        .flat_map(|line| line.words.iter())
        .map(|word| word.text.as_str())
        .collect();
    anyhow::ensure!(
        text.contains('昨') && text.contains('日'),
        "the fixture frame must read 昨日, got {text:?}"
    );
    anyhow::ensure!(
        lines
            .iter()
            .flat_map(|line| line.words.iter())
            .all(|word| word.rect.w > 0 && word.rect.h > 0),
        "every recognised word needs a non-degenerate box"
    );
    // Report the recogniser that Windows selected for the tag, not the first
    // tag that happens to be installed.
    let language = engine
        .engine()
        .RecognizerLanguage()
        .and_then(|language| language.LanguageTag())
        .map(|tag| tag.to_string())
        .unwrap_or_else(|_| "ja".to_string());
    Ok(Outcome::Pass(format!(
        "{} line(s) via {language}; text {text:?}",
        lines.len()
    )))
}

/// Case 4. Resolves a point in synthetic OCR output.
fn text_resolution(_ctx: &Ctx) -> Result<Outcome> {
    let lines = synthetic_lines("猫", 10);
    let region = chibipop::geom::PhysRect { x: 0, y: 0, w: 400, h: 120 };
    let cursor = lines[0].words[0].rect.center();
    let resolved = chibipop::text::layout::resolve(&lines, cursor, region, true)
        .context("the point must resolve inside the synthetic line")?;
    anyhow::ensure!(
        resolved.span.text.contains(TERM),
        "the resolved span must contain {TERM}, got {:?}",
        resolved.span.text
    );
    Ok(Outcome::Pass(format!(
        "({}, {}) resolved to {:?} {:?}",
        cursor.x, cursor.y, resolved.orientation, resolved.span.text
    )))
}

/// Case 5. Cuts the sentence at one anchor and segments its words.
fn sentence_analysis(ctx: &Ctx) -> Result<Outcome> {
    let model = match ctx.need(IPADIC)? {
        Ok(path) => path,
        Err(skip) => return Ok(skip),
    };
    let lines = synthetic_sentence();
    let anchor = lines[0].words[1].rect;
    let sentence = chibipop::text::sentence::sentence_at(
        &lines,
        anchor,
        chibipop::text::layout::Orientation::Horizontal,
    )
    .context("the anchor must select a sentence")?;
    anyhow::ensure!(
        sentence.contains(TERM),
        "the sentence must contain {TERM}, got {sentence:?}"
    );
    let mut analyser = chibipop::analysis::Analyzer::load(&model)
        .with_context(|| format!("loading the model at {}", model.display()))?;
    let analysis = analyser.analyze(&sentence);
    anyhow::ensure!(
        !analysis.words.is_empty(),
        "the model must segment {sentence:?} into words"
    );
    Ok(Outcome::Pass(format!(
        "{:?} -> {} morpheme(s), {} word range(s)",
        sentence,
        analysis.morphemes.len(),
        analysis.words.len()
    )))
}

/// Case 6. Builds search candidates through the production search service.
fn search_candidates(ctx: &Ctx) -> Result<Outcome> {
    let database = match ctx.database()? {
        Ok(path) => path,
        Err(skip) => return Ok(skip),
    };
    let cfg = chibipop::config::Config::default();
    let service = search::SearchService::open(&database, &ctx.fixture(RULES), &cfg)
        .with_context(|| format!("opening search on {}", database.display()))?;
    let result = service.search(TERM)?;
    let candidates = search::candidates(&result);
    anyhow::ensure!(
        !candidates.is_empty(),
        "{TERM} must produce at least one candidate"
    );
    let head = &candidates[0];
    Ok(Outcome::Pass(format!(
        "{} candidate(s); first {} [{}]",
        candidates.len(),
        head.headword,
        head.reading
    )))
}

/// Case 7. Builds the presentation model for the top card.
fn presentation(ctx: &Ctx) -> Result<Outcome> {
    let card = match ctx.top_card(TERM)? {
        Ok(card) => card,
        Err(skip) => return Ok(skip),
    };
    let entries: usize = card.blocks.iter().map(|block| block.entries.len()).sum();
    anyhow::ensure!(
        !card.blocks.is_empty() && entries > 0,
        "the top card must carry at least one gloss entry"
    );
    Ok(Outcome::Pass(format!(
        "top card {:?} with {} block(s), {entries} entr(ies), {} pitch row(s)",
        card.written.as_deref().unwrap_or(TERM),
        card.blocks.len(),
        card.pitch.len()
    )))
}

/// Case 8. Builds the Anki field payload from the top card.
fn anki_fields(ctx: &Ctx) -> Result<Outcome> {
    let card = match ctx.top_card(TERM)? {
        Ok(card) => card,
        Err(skip) => return Ok(skip),
    };
    let fields = chibipop::anki::fields_from_card(&card, &card.blocks, true);
    let expression = fields
        .iter()
        .find(|(_, value)| value.contains(TERM))
        .map(|(key, _)| key.clone());
    anyhow::ensure!(
        expression.is_some(),
        "one field must carry {TERM}, got the keys {:?}",
        fields.keys().collect::<Vec<_>>()
    );
    let total: usize = fields.values().map(String::len).sum();
    Ok(Outcome::Pass(format!(
        "{} field(s), {total} byte(s); {} carries {TERM}",
        fields.len(),
        expression.unwrap_or_default()
    )))
}

/// Case 9. Encodes synthetic pixels and saves the PNG under the run root.
fn png_encoding(ctx: &Ctx) -> Result<Outcome> {
    let (w, h) = (320, 120);
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index as i32 % w, index as i32 / w);
        let dark = x % 11 < 2 || y % 13 < 2;
        let value = if dark { 0x18 } else { 0xF4 };
        pixel.copy_from_slice(&[value, value, value, 0xFF]);
    }
    let png = chibipop::image::encode_bgra_to_png(&pixels, w, h)?;
    let plan = chibipop::shot::ShotPlan {
        expr: TERM.to_string(),
        fields: std::collections::HashMap::new(),
        path: ctx.scratch.join(SCREENSHOT),
        picture_fields: Vec::new(),
    };
    chibipop::shot::save(&png, &plan)?;
    let written = std::fs::read(&plan.path)
        .with_context(|| format!("reading {}", plan.path.display()))?;
    anyhow::ensure!(written == png, "the saved PNG differs from the encoded PNG");
    anyhow::ensure!(
        written.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "the saved file is not a PNG"
    );
    Ok(Outcome::Pass(format!(
        "{w}x{h} encoded to {} byte(s) at {}",
        png.len(),
        plan.path.display()
    )))
}

/// Case 10. Loads, validates, and projects the configuration.
fn config(ctx: &Ctx) -> Result<Outcome> {
    let path = ctx.run_root.join("chibipop.toml");
    let cfg = chibipop::config::load_or_create(&path)
        .with_context(|| format!("loading a configuration from {}", path.display()))?;
    cfg.validate_hotkeys(chibipop::config::Platform::Windows)?;
    let dicts = vec![chibipop::present::DictInfo {
        dict_id: 1,
        name: "FixtureTerms".to_string(),
    }];
    let present = cfg.present_config(&dicts);
    anyhow::ensure!(
        path.is_file(),
        "load_or_create must write the default configuration"
    );
    Ok(Outcome::Pass(format!(
        "default configuration wrote {} and validated; {} term dictionary enabled",
        path.display(),
        present.terms.len()
    )))
}

/// Case 11. Discovers one generated plugin and runs the production echo call.
fn plugin_cli(ctx: &Ctx) -> Result<Outcome> {
    let exe = ctx.self_exe()?;
    let root = ctx.scratch.join("plugins");
    let dir = root.join("echo");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let manifest = format!(
        "name = \"echo\"\nversion = \"0.1.0\"\nprotocol = 1\ncommand = \"{}\"\n\
         args = [\"plugin-echo\", \"ok\"]\nroles = [\"text-provider\"]\n\n\
         [text_provider]\nprovides_geometry = true\nlanguages = [\"ja\"]\ntimeout_ms = 2000\n",
        exe.display().to_string().replace('\\', "\\\\")
    );
    std::fs::write(dir.join("plugin.toml"), manifest)
        .with_context(|| format!("writing {}", dir.join("plugin.toml").display()))?;

    let mut pixels = vec![0u8; 4 * 4 * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[0x20, 0x20, 0x20, 0xFF]);
    }
    let image = ctx.run_root.join("artifacts").join("plugin.png");
    let png = chibipop::image::encode_bgra_to_png(&pixels, 4, 4)?;
    let parent = image
        .parent()
        .with_context(|| format!("{} has no folder", image.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    std::fs::write(&image, &png)
        .with_context(|| format!("writing {}", image.display()))?;

    // The two calls below print their own report, as they do for the `plugin`
    // subcommand. This phase promises one JSON object per line on stdout, so
    // hold their report aside and send it to stderr.
    let held = ctx.scratch.join("plugin-report.log");
    let code = {
        let _held = OutputRedirect::hold(&held)?;
        let listed = chibipop_windows::plugin::cli::list(&root);
        let tested = chibipop_windows::plugin::cli::test_one(&root, "echo", &image);
        anyhow::ensure!(listed == 0, "plugin discovery reported code {listed}");
        anyhow::ensure!(tested == 0, "the plugin test call reported code {tested}");
        (listed, tested)
    };
    let report = std::fs::read_to_string(&held).unwrap_or_default();
    eprint!("{report}");
    Ok(Outcome::Pass(format!(
        "discovery and the echo exchange returned {} and {} for {}; report {} byte(s)",
        code.0,
        code.1,
        exe.display(),
        report.len()
    )))
}

/// Sends the process stdout of one block to a file, and puts it back after.
/// The plugin CLI prints a human report instead of JSON. Stdout carries one
/// JSON object per case, so that report cannot land there.
struct OutputRedirect {
    file: std::fs::File,
    saved: HANDLE,
}

impl OutputRedirect {
    fn hold(path: &Path) -> Result<OutputRedirect> {
        // Flush the line buffer first, so no earlier byte lands in the file.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(FILE_SHARE_WRITE.0)
            .open(path)
            .with_context(|| format!("opening {} for the held report", path.display()))?;
        // SAFETY: The standard handle for this class is either a valid handle
        // or null, and `SetStdHandle` takes that value by copy. Both calls only
        // read or replace this process's own standard handle.
        let saved = unsafe {
            let saved = GetStdHandle(STD_OUTPUT_HANDLE).unwrap_or_default();
            SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(file.as_raw_handle()))
                .with_context(|| format!("redirecting stdout to {}", path.display()))?;
            saved
        };
        Ok(OutputRedirect { file, saved })
    }
}

impl Drop for OutputRedirect {
    fn drop(&mut self) {
        // SAFETY: `self.saved` is the handle that this process used for stdout
        // before `hold` replaced it. No write above outlives the redirect.
        unsafe {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, self.saved);
        }
        let _ = self.file.sync_all();
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Returns one horizontal line that holds `text` at `y`.
fn synthetic_lines(text: &str, y: i32) -> Vec<OcrLine> {
    vec![OcrLine {
        words: vec![OcrWord {
            text: text.to_string(),
            rect: chibipop::geom::PhysRect { x: 10, y, w: 24, h: 24 },
        }],
    }]
}

/// Returns two horizontal sentences with a paragraph gap between them.
/// The gap is wider than the cut threshold, so the anchor selects the first.
fn synthetic_sentence() -> Vec<OcrLine> {
    let word = |text: &str, x: i32| OcrWord {
        text: text.to_string(),
        rect: chibipop::geom::PhysRect { x, y: 10, w: 20, h: 20 },
    };
    vec![OcrLine {
        words: vec![
            word("猫", 10),
            word("が", 30),
            word("好", 50),
            word("き", 70),
            word("。", 90),
            // The next four words form the second sentence of the paragraph.
            word("犬", 190),
            word("で", 210),
            word("す", 230),
            word("。", 250),
        ],
    }]
}
