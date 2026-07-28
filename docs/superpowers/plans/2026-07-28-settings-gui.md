# Settings GUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put all eleven settings behind `Settings…` on the tray menu, in a modeless native window whose Apply persists and restarts chibipop — and give the executable its icon on the way past.

**Architecture:** A pure `src/settings.rs` converts a `Config` plus the live dictionary list into plain form data and back, including the substring-preserving dictionary write-back; it never sees an `HWND`. `src/ui/settings_window.rs` owns the window and its controls and speaks only that plain data. `run` keeps ownership of loading, saving, spawning and quitting, and routes messages to the window through `IsDialogMessageW` so no nested pump is ever created.

**Tech Stack:** Rust 2021, `windows` 0.62.2, predefined Win32 control classes, a checked-in `.res` linked by a three-line `build.rs`. No new crate.

**Spec:** `docs/superpowers/specs/2026-07-28-settings-gui-design.md` (at `a302813`, including D6a)

## Global Constraints

- **`src/geom.rs`, `src/text/layout.rs`, `src/lookup/`, `src/present.rs`, `src/config.rs`, `src/settings.rs` and `src/ui/theme.rs` must not depend on the `windows` crate.** They compile and test with no screen. `src/settings.rs` is new and joins that list.
- **Comments:** default is none; non-doc comment text **under 30 characters**. Rustdoc (`///`, `//!`) and `// SAFETY:` are exempt and expected to be detailed. Every `unsafe` block gets a `// SAFETY:`. **Over-budget rationale gets relocated to rustdoc, never deleted.**
- **Do NOT run `cargo fmt`.** This repo has never been rustfmt-clean.
- **Clippy must stay at exactly 5 accepted errors** (`app.rs`, `input/hooks.rs`, `lookup/deconj.rs`, `lookup/model.rs`, `ui/render.rs`). **Check it in every task.** The usual command aborts on those 5 before the *binary* target compiles, so to lint `src/main.rs` also run it with them suppressed:
  `-A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity`
- **A task that adds a struct field, a constant or an import must also be the task that reads it.** Otherwise the intermediate commit trips `field never read` / `unused import` and cannot pass the clippy gate. This bit the previous round twice.
- **Before extending any struct, grep for every construction site**, test helpers included — a struct literal has no partial form. This was a Critical in the previous round.
- **Cargo is not on PATH.** Prefix every cargo command with:
  `export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup;`
- **chibipop may hold a lock on its own binary.** On "failed to remove file":
  `powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"`
- **`Cargo.toml` has a pre-existing unrelated unstaged change. Leave it; never stage it.** Task 1 *does* edit `Cargo.toml` (adding `build = "build.rs"`) — stage that file **only** in Task 1, and check `git diff --cached Cargo.toml` shows only the `build` key before committing.
- **Never `git add -A`, `git add .`, or `git add -u`.** Stage only the files you deliberately changed, by name.
- **Git identity is set repo-locally.** Do not change it or pass `--author`.
- **Screen constraint:** anything opening a window uses the **portrait secondary monitor (x ≥ 2560)**.
- **Synthetic input cannot reach chibipop's `WH_MOUSE_LL` hook.** `SendInput` from any tool-spawned shell returns **0**. **No task may claim a UI behaviour was verified by clicking it** — the tray menu cannot be opened and no control can be pressed by an agent. Task 7 covers what is and is not provable.
- **Test totals drift.** Never hard-code a total without running the suite and reading it off.

**Task order.** Task 1 is independent and lands first (it is valuable on its own — the exe gets an icon). Tasks 2 and 3 are prerequisites for the window and are pure/near-pure. Tasks 4–6 build the window incrementally, each leaving a working app.

---

### Task 1: The manifest, and the executable icon

**Files:**
- Create: `build.rs`, `assets/chibipop.rc`, `assets/chibipop.manifest`, `assets/chibipop.res` (binary, generated once)
- Modify: `Cargo.toml` (add `build = "build.rs"`), `docs/BACKLOG.md` (close item 4)

**Interfaces:**
- Produces: a linked resource giving the exe its icon and declaring comctl32 v6, so predefined controls render themed rather than in Windows-95 grey.

**Independent of every other task.** Nothing here touches Rust source.

⚠️ **Do NOT put DPI awareness in the manifest.** `text::capture::init_dpi_awareness()` calls `SetProcessDpiAwarenessContext` at runtime, and that call is **documented to fail once a process's DPI awareness is already established** — `app.rs`'s module docs say so explicitly. A manifest sets it before `main` runs, so adding `<dpiAware>` would silently break the runtime call the whole capture path depends on. Common Controls only.

- [ ] **Step 1: Write the manifest**

Create `assets/chibipop.manifest`:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="chibipop" version="1.0.0.0" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
          type="win32"
          name="Microsoft.Windows.Common-Controls"
          version="6.0.0.0"
          processorArchitecture="*"
          publicKeyToken="6595b64144ccf1df"
          language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
```

- [ ] **Step 2: Write the resource script**

Create `assets/chibipop.rc`:

```
#define IDI_CHIBIPOP 1
#define CREATEPROCESS_MANIFEST_RESOURCE_ID 1
#define RT_MANIFEST 24

IDI_CHIBIPOP ICON "chibipop.ico"
CREATEPROCESS_MANIFEST_RESOURCE_ID RT_MANIFEST "chibipop.manifest"
```

Resource id `1` for the icon matters: Explorer shows the **lowest-numbered** icon resource as the file's icon.

- [ ] **Step 3: Compile the .res, from PowerShell**

`rc.exe` fails under git-bash — MSYS2 mangles its `/flag` arguments. Find it and run it from PowerShell:

```powershell
$rc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter rc.exe |
      Where-Object { $_.FullName -like "*x64*" } | Select-Object -Last 1
& $rc.FullName /nologo /fo C:\Users\Stella\chibipop\assets\chibipop.res C:\Users\Stella\chibipop\assets\chibipop.rc
```

Expected: `chibipop.res` created, a few KB larger than `chibipop.ico`. If `rc.exe` is not found, **stop and report** — do not add a crate to work around it.

- [ ] **Step 4: Write build.rs**

Create `build.rs` at the repository root:

```rust
//! Links the checked-in Windows resource: the executable's icon, and the
//! application manifest that asks for Common Controls v6.
//!
//! The `.res` is committed rather than compiled here on purpose. Invoking
//! `rc.exe` from a build script would make every build depend on locating a
//! Windows SDK, and this project has taken no build dependencies since it
//! started. Regenerate it with the PowerShell command in
//! `docs/superpowers/plans/2026-07-28-settings-gui.md` Task 1 Step 3 whenever
//! `assets/chibipop.ico` or `assets/chibipop.manifest` changes.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets this");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.res");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.manifest");
    println!("cargo:rustc-link-arg-bins={dir}/assets/chibipop.res");
}
```

`rustc-link-arg-bins`, not `rustc-link-arg`: the latter also applies to test and doctest link steps, where a resource file has no business.

- [ ] **Step 5: Point Cargo at it**

In `Cargo.toml`'s `[package]` section add:

```toml
build = "build.rs"
```

- [ ] **Step 6: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo build --release 2>&1 | grep -E "^error|Finished"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
```

Then confirm the resource actually linked — the binary should grow by roughly the `.ico`'s size:

```bash
ls -l target/release/chibipop.exe assets/chibipop.ico assets/chibipop.res
```

Report the before/after exe size. A build that succeeds but does not grow means the link arg was ignored.

- [ ] **Step 7: Close the backlog item and commit**

In `docs/BACKLOG.md`, replace the "Executable icon" bullet under "Smaller carried items" with a note that it shipped in this round, naming the commit.

```bash
git add build.rs Cargo.toml assets/chibipop.rc assets/chibipop.manifest assets/chibipop.res docs/BACKLOG.md
git commit -m "build: link the app manifest and the executable icon"
```

**Before committing, run `git diff --cached Cargo.toml`** and confirm it shows *only* the added `build` key — the pre-existing unstaged reformat must not be swept in.

---

### Task 2: The main thread learns the dictionary list

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Produces: `startup_tx`/`startup_rx` carry `Result<Vec<DictInfo>>` instead of `Result<()>`; `run` holds `dicts: Vec<DictInfo>` for the process's lifetime.

**Spec D7.** The settings window must list real dictionary names, and `SqliteDictionary` lives on the worker thread. The worker already reads `dicts()` once at startup ("Decision 2"); this hands that result to the main thread rather than opening a second connection there.

- [ ] **Step 1: Change the channel type**

In `run`, change:

```rust
    let (startup_tx, startup_rx) = mpsc::channel::<Result<()>>();
```

to:

```rust
    let (startup_tx, startup_rx) = mpsc::channel::<Result<Vec<DictInfo>>>();
```

- [ ] **Step 2: Keep the payload at the receive site**

Change:

```rust
    startup_rx
        .recv()
        .context("worker thread ended before completing startup")??;
```

to:

```rust
    // Spec D7: the worker reads these once at startup and the main thread
    // needs them for the settings window's dictionary list.
    let dicts: Vec<DictInfo> = startup_rx
        .recv()
        .context("worker thread ended before completing startup")??;
```

- [ ] **Step 3: Send the payload**

In `worker_main`, change the parameter type:

```rust
    startup_tx: mpsc::Sender<Result<Vec<DictInfo>>>,
```

and the success send, which currently reads `if startup_tx.send(Ok(())).is_err()`:

```rust
    if startup_tx.send(Ok(dicts.clone())).is_err() {
        return; // main thread gave up waiting; nothing left to do.
    }
```

`clone` because the worker keeps its own copy for every later `present::build`. Two dictionary identities are a handful of bytes; sharing them behind an `Arc` for that would be ceremony.

The four `startup_tx.send(Err(e))` sites need no change — `Err` is generic over the payload.

- [ ] **Step 4: Silence the unused binding until Task 4 uses it**

`dicts` is not read until Task 4, and an unused binding is a clippy error against the exactly-5 gate. Bind it as `_dicts` in this task and rename it in Task 4 Step 3, where the settings window consumes it. Note the rename in Task 4 so it is not forgotten.

- [ ] **Step 5: Verify and commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo build --release 2>&1 | grep -E "^error|Finished"
```

```bash
git add src/app.rs
git commit -m "refactor(app): carry the dictionary list across the startup handshake"
```

---

### Task 3: The pure settings model

**Files:**
- Create: `src/settings.rs`
- Modify: `src/lib.rs` (add `pub mod settings;`), `src/present.rs` (make `dict_order_rank` public)

**Interfaces:**
- Consumes: `Config`, `TriggerMode` from `config.rs`; `DictInfo` from `present.rs`.
- Produces:
  - `pub struct SettingsForm { … }` — the eleven settings as plain data
  - `pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm`
  - `pub fn apply_to(form: &SettingsForm, cfg: &Config) -> Config`
  - `pub fn stale_order_entries(cfg: &Config, dicts: &[DictInfo]) -> Vec<String>`
  - `pub const MAX_HEIGHT_RANGE: (u8, u8)`, `SUMMARY_RANGE: (usize, usize)`, `PASSES_RANGE: (u8, u8)`

**No `windows` dependency.** This is where every interesting decision lives and where all of this round's automated coverage sits.

- [ ] **Step 1: Make the ordering rule shareable**

In `src/present.rs`, change `fn dict_order_rank` to `pub fn dict_order_rank` and extend its doc comment with:

```rust
/// Public so the settings window orders its dictionary list by exactly the
/// rule the popup orders blocks by. A window that computed its own ordering
/// could disagree with what the user then sees, which is worse than not
/// showing an order at all.
```

- [ ] **Step 2: Write the failing tests**

Create `src/settings.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TriggerMode;

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    fn cfg_with(order: &[&str]) -> Config {
        let mut c = Config::default();
        c.dictionaries.display_order = order.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn the_form_lists_dictionaries_in_the_configured_order() {
        let form = from_config(&cfg_with(&["大辞林", "Jitendex"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
    }

    /// A dictionary matching no entry sorts last, by dict_id - the same rule
    /// `present::build` applies, so the window cannot show an order the popup
    /// will not honour.
    #[test]
    fn an_unordered_dictionary_is_listed_last() {
        let form = from_config(&cfg_with(&["大辞林"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
    }

    /// THE trap (spec D6). `display_order` holds substrings because
    /// Jitendex's name carries a release date that changes on every rebuild.
    /// Reordering must return the user's own strings, never the live names.
    #[test]
    fn reordering_preserves_the_existing_substrings_verbatim() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let mut form = from_config(&cfg, &dicts());
        form.dict_names.reverse();
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["Jitendex".to_string(), "大辞林".to_string()],
                   out.dictionaries.display_order);
    }

    #[test]
    fn a_never_ordered_dictionary_derives_a_key_without_the_date() {
        let cfg = cfg_with(&[]);
        let form = from_config(&cfg, &dicts());
        let out = apply_to(&form, &cfg);
        assert!(out.dictionaries.display_order.contains(&"Jitendex.org".to_string()),
                "got {:?}", out.dictionaries.display_order);
        assert!(out.dictionaries.display_order.contains(&"大辞林　第四版".to_string()));
    }

    /// A name that is entirely bracketed would derive an empty key, which
    /// would then match every dictionary.
    #[test]
    fn a_key_that_would_be_empty_falls_back_to_the_whole_name() {
        let cfg = cfg_with(&[]);
        let odd = vec![DictInfo { dict_id: 1, name: "[2026] Something".into() }];
        let form = from_config(&cfg, &odd);
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["[2026] Something".to_string()], out.dictionaries.display_order);
    }

    /// D6a: an entry matching nothing is what a rename looks like from here.
    #[test]
    fn an_entry_matching_no_dictionary_is_reported() {
        let stale = stale_order_entries(&cfg_with(&["大辞林", "Kenkyusha"]), &dicts());
        assert_eq!(vec!["Kenkyusha".to_string()], stale);
    }

    #[test]
    fn entries_that_all_match_report_nothing() {
        assert!(stale_order_entries(&cfg_with(&["大辞林", "Jitendex"]), &dicts()).is_empty());
    }

    /// The window must never silently change a setting the user did not
    /// touch. This is the assertion that catches a mis-wired control.
    #[test]
    fn an_untouched_form_round_trips_to_an_equal_config() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn every_setting_survives_the_round_trip() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
        cfg.trigger.mode = TriggerMode::HoldShift;
        cfg.popup.theme = "light".into();
        cfg.popup.font = "Noto Sans JP".into();
        cfg.popup.max_height_percent = 70;
        cfg.popup.summary_chars = 25;
        cfg.popup.highlight_match = false;
        cfg.popup.scroll_popup = false;
        cfg.popup.exclude_from_capture = true;
        cfg.ocr.max_ocr_passes = 3;
        cfg.debug.show_scan_region = true;
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn out_of_range_numbers_are_clamped_not_rejected() {
        let cfg = cfg_with(&[]);
        let mut form = from_config(&cfg, &dicts());
        form.max_height_percent = 250;
        form.summary_chars = 1;
        form.max_ocr_passes = 99;
        let out = apply_to(&form, &cfg);
        assert_eq!(MAX_HEIGHT_RANGE.1, out.popup.max_height_percent);
        assert_eq!(SUMMARY_RANGE.0, out.popup.summary_chars);
        assert_eq!(PASSES_RANGE.1, out.ocr.max_ocr_passes);
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Add `pub mod settings;` to `src/lib.rs` first, then:

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib settings 2>&1 | tail -20
```

Expected: `cannot find function 'from_config' in this scope`. **Paste the real output — a paraphrased RED transcript is a task failure.**

- [ ] **Step 4: Write the implementation**

Put this **above** the test module in `src/settings.rs`:

```rust
//! The settings window's data model: a `Config` flattened into plain fields
//! and folded back again.
//!
//! Deliberately free of the `windows` crate, so every decision this round
//! makes that is worth testing can be tested without a screen. `ui`'s
//! settings window owns the controls and speaks only these types; it holds no
//! opinion about what any of them mean.

use crate::config::{Config, TriggerMode};
use crate::present::{dict_order_rank, DictInfo};

/// Popup height cap, as a percentage of the monitor's height.
pub const MAX_HEIGHT_RANGE: (u8, u8) = (10, 90);
/// Collapsed-row summary length, in characters.
pub const SUMMARY_RANGE: (usize, usize) = (10, 200);
/// OCR captures per hover. 1 disables forward tiling.
pub const PASSES_RANGE: (u8, u8) = (1, 5);

/// Every user-facing setting, as the window edits it.
///
/// `dict_names` holds **live dictionary names**, not the substrings the
/// config stores — the window shows the user what is installed, and
/// [`apply_to`] converts back. See its documentation for why that conversion
/// is not the identity.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    pub mode: TriggerMode,
    pub theme: String,
    pub font: String,
    pub max_height_percent: u8,
    pub summary_chars: usize,
    pub highlight_match: bool,
    pub scroll_popup: bool,
    pub exclude_from_capture: bool,
    pub dict_names: Vec<String>,
    pub max_ocr_passes: u8,
    pub show_scan_region: bool,
}

/// Flatten `cfg` for editing, listing `dicts` in the order the popup would
/// actually show them.
///
/// The ordering runs through [`dict_order_rank`], the same function
/// `present::build` uses, so the list cannot show an order the popup will not
/// honour.
pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm {
    let mut ordered: Vec<&DictInfo> = dicts.iter().collect();
    ordered.sort_by_key(|d| {
        (
            dict_order_rank(&d.name, &cfg.dictionaries.display_order).unwrap_or(usize::MAX),
            d.dict_id,
        )
    });

    SettingsForm {
        mode: cfg.trigger.mode,
        theme: cfg.popup.theme.clone(),
        font: cfg.popup.font.clone(),
        max_height_percent: cfg.popup.max_height_percent,
        summary_chars: cfg.popup.summary_chars,
        highlight_match: cfg.popup.highlight_match,
        scroll_popup: cfg.popup.scroll_popup,
        exclude_from_capture: cfg.popup.exclude_from_capture,
        dict_names: ordered.iter().map(|d| d.name.clone()).collect(),
        max_ocr_passes: cfg.ocr.max_ocr_passes,
        show_scan_region: cfg.debug.show_scan_region,
    }
}

/// Fold `form` back onto `cfg`, returning the config to write.
///
/// **`display_order` is rebuilt from substrings, never from live names**, and
/// that is the whole reason this is not a field-for-field copy.
/// `dictionaries.display_order` holds case-insensitive *substrings* because
/// Jitendex's stored name embeds a release date that changes on every
/// rebuild; writing live names back would work perfectly until the next
/// rebuild and then silently stop applying, dropping that dictionary to the
/// bottom with no error anywhere. See [`order_key`].
///
/// Numbers are clamped rather than rejected: the window's spin controls
/// cannot produce an out-of-range value, so a value that is out of range came
/// from a hand-edited file, and quietly bringing it into range beats refusing
/// to apply anything.
pub fn apply_to(form: &SettingsForm, cfg: &Config) -> Config {
    let mut out = cfg.clone();
    out.trigger.mode = form.mode;
    out.popup.theme = form.theme.clone();
    out.popup.font = form.font.clone();
    out.popup.max_height_percent =
        form.max_height_percent.clamp(MAX_HEIGHT_RANGE.0, MAX_HEIGHT_RANGE.1);
    out.popup.summary_chars = form.summary_chars.clamp(SUMMARY_RANGE.0, SUMMARY_RANGE.1);
    out.popup.highlight_match = form.highlight_match;
    out.popup.scroll_popup = form.scroll_popup;
    out.popup.exclude_from_capture = form.exclude_from_capture;
    out.ocr.max_ocr_passes = form.max_ocr_passes.clamp(PASSES_RANGE.0, PASSES_RANGE.1);
    out.debug.show_scan_region = form.show_scan_region;
    out.dictionaries.display_order = form
        .dict_names
        .iter()
        .map(|name| order_key(name, &cfg.dictionaries.display_order))
        .collect();
    out
}

/// The `display_order` entry to write for a dictionary called `name`.
///
/// Keeps whichever existing entry already matches, **verbatim**, so a config
/// the user has been running keeps working across dictionary rebuilds. Only a
/// dictionary that has never been ordered gets a derived key, truncated at
/// the first bracket so a release date cannot become part of it.
///
/// Falls back to the whole name when truncation would leave nothing — an
/// empty entry would match *every* dictionary and silently pin the order.
fn order_key(name: &str, existing: &[String]) -> String {
    let lower = name.to_lowercase();
    if let Some(entry) = existing.iter().find(|e| lower.contains(&e.to_lowercase())) {
        return entry.clone();
    }
    let cut = name.find(['[', '(']).unwrap_or(name.len());
    let derived = name[..cut].trim();
    if derived.is_empty() { name.to_string() } else { derived.to_string() }
}

/// `display_order` entries matching no installed dictionary (spec D6a).
///
/// This is what a dictionary rename looks like from inside the settings
/// window, and it is worth reporting precisely: `dict_order_rank` returns
/// `None` for an unmatched dictionary and the caller sorts it **last**, so
/// the user-visible symptom is a dictionary silently dropping to the bottom —
/// which reads as chibipop reordering things by itself rather than as a stale
/// line in a file.
pub fn stale_order_entries(cfg: &Config, dicts: &[DictInfo]) -> Vec<String> {
    cfg.dictionaries
        .display_order
        .iter()
        .filter(|entry| {
            let needle = entry.to_lowercase();
            !dicts.iter().any(|d| d.name.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib settings 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 6: Prove the substring rule is load-bearing**

Replace `order_key`'s body with `name.to_string()` — the naive "write live names back". Re-run and confirm `reordering_preserves_the_existing_substrings_verbatim` **FAILS**. Restore and confirm it passes. Capture both outputs verbatim. This is the round's central trap; a test that passes either way does not protect it.

- [ ] **Step 7: Check clippy and commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
git add src/settings.rs src/lib.rs src/present.rs
git commit -m "feat(settings): the pure model behind the settings window"
```

---

### Task 4: The window, and the tray menu that opens it

**Files:**
- Create: `src/ui/settings_window.rs`
- Modify: `src/ui/mod.rs`, `src/ui/tray.rs`, `src/app.rs`

**Interfaces:**
- Consumes: `SettingsForm`, `from_config`, `stale_order_entries` (Task 3); `dicts` (Task 2).
- Produces:
  - `pub struct SettingsWindow` with `pub fn open(form: SettingsForm, stale: Vec<String>, font: &str) -> Result<SettingsWindow>`, `pub fn hwnd(&self) -> HWND`, `pub fn read(&self) -> SettingsForm`, `pub fn take_outcome(&self) -> Option<SettingsOutcome>`
  - `pub enum SettingsOutcome { Apply, Cancel }`
  - `TrayCommand::OpenSettings`, replacing `TrayCommand::SetMode`

This task delivers a window that **opens, shows the controls, and closes** — Apply does not yet restart (Task 6). Leaving it non-functional for one task is deliberate: the window is large enough that wiring it and restarting in one commit would make a review of either half impossible.

- [ ] **Step 1: Replace the tray's mode items**

In `src/ui/tray.rs`:

- Replace `const ID_MODE_LIVE: u32 = 1001;` and `const ID_MODE_HOLD_SHIFT: u32 = 1002;` with `const ID_SETTINGS: u32 = 1001;`. Keep `ID_QUIT` at 1003.
- Change `TrayCommand` to:

```rust
pub enum TrayCommand {
    OpenSettings,
    Quit,
}
```

- In `build_menu`, replace the two `append_radio_item` calls and the separator with:

```rust
    AppendMenuW(hmenu, MF_STRING, ID_SETTINGS as usize, w!("Settings…"))
        .context("AppendMenuW Settings")?;
    AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).context("AppendMenuW separator")?;
    AppendMenuW(hmenu, MF_STRING, ID_QUIT as usize, w!("Quit")).context("AppendMenuW Quit")?;
```

- Delete `append_radio_item` and the now-unused `mode` field, `Cell` import, `set_mode` method and `TriggerMode` import from `tray.rs`. `build_menu` no longer takes a mode. **Grep for every use of each before deleting** — `Tray::create` takes `initial_mode` and `app.rs` passes it.
- In `show_menu`'s match, replace the two mode arms with:

```rust
                ID_SETTINGS => Some(TrayCommand::OpenSettings),
```

- [ ] **Step 2: Create the window module**

Create `src/ui/settings_window.rs`. It owns one top-level window plus its child controls, and is the only place in the crate that touches control classes.

```rust
//! The settings window: eleven settings, a dictionary order, Apply and
//! Cancel.
//!
//! **Modeless, and that is a safety property rather than a preference.**
//! `DialogBoxParamW` runs its own message pump, and a nested pump on the main
//! thread stops `WM_TIMER` arriving while the low-level hook keeps firing —
//! which latches `input::hooks::SCROLL_ARMED` and kills the scroll wheel for
//! every application (popup-interaction spec D9, reproduced from
//! `TrackPopupMenuEx`). A tray menu holds that for a second; a settings
//! window would hold it for minutes. So this is an ordinary `CreateWindowExW`
//! window serviced by `app::run`'s existing loop through `IsDialogMessageW`.
//!
//! It holds no opinion about what any setting means: it is handed a
//! `settings::SettingsForm`, and hands one back.

use crate::settings::{SettingsForm, MAX_HEIGHT_RANGE, PASSES_RANGE, SUMMARY_RANGE};
use anyhow::{Context, Result};
use std::cell::Cell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// What the user did with the window. Read and cleared by `app::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    Apply,
    Cancel,
}
```

The control ids, the class registration, `open`, `read` and `take_outcome` follow the same shape `ui/overlay.rs` already uses for a thread-local paint state — copy that structure rather than inventing a second one.

**Because this is the longest single file in the round, build it in the order below and compile after each group**, rather than writing all eleven controls and then debugging a wall of errors:

1. Window class + empty window + `open`/`hwnd`/`Drop`.
2. The Apply and Cancel buttons, `WM_COMMAND` routing, `take_outcome`.
3. The checkboxes and radios.
4. The two combos (theme, font).
5. The three spin/edit pairs.
6. The dictionary listbox and its Move up / Move down buttons.
7. The static hint and the D6a warning label.

- [ ] **Step 3: Open it from `run`**

In `src/app.rs`:

- Rename Task 2's `_dicts` binding to `dicts`.
- Add `let mut settings: Option<SettingsWindow> = None;` beside the other loop state.
- In the message loop, **before** `TranslateMessage`, add the modeless-dialog branch:

```rust
        if let Some(w) = &settings {
            // Modeless: this is what gives Tab, arrows and Escape without a
            // nested pump. See settings_window's module docs.
            if unsafe { IsDialogMessageW(w.hwnd(), &msg) }.as_bool() {
                continue;
            }
        }
```

- Replace the `TrayCommand::SetMode` arm with:

```rust
                TrayCommand::OpenSettings => {
                    if let Some(w) = &settings {
                        unsafe {
                            let _ = SetForegroundWindow(w.hwnd());
                        }
                    } else {
                        let form = settings::from_config(&cfg, &dicts);
                        let stale = settings::stale_order_entries(&cfg, &dicts);
                        match SettingsWindow::open(form, stale, &cfg.popup.font) {
                            Ok(w) => settings = Some(w),
                            Err(e) => eprintln!("chibipop: opening settings failed: {e:#}"),
                        }
                    }
                }
```

Window creation failing is logged and survivable — the same rule the overlay follows. Settings is not worth killing the app for.

- `Hooks::set_mode(cfg.trigger.mode)` stays at startup; nothing calls it later now.

- [ ] **Step 4: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo clippy --all-targets --all-features -- -D warnings -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -E "^error|^warning" | head
cargo build --release 2>&1 | grep -E "^error|Finished"
```

Then prove the window can be created at all, without a tray click — start `run` from PowerShell and check the class exists:

```powershell
Start-Process -FilePath "C:\Users\Stella\chibipop\target\release\chibipop.exe" -ArgumentList "run" -WorkingDirectory "C:\Users\Stella\chibipop" -WindowStyle Hidden
Start-Sleep -Seconds 3
Add-Type @'
using System;using System.Text;using System.Runtime.InteropServices;
public class W { [DllImport("user32.dll")] public static extern bool EnumWindows(EP f, IntPtr l);
 [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
 public delegate bool EP(IntPtr h, IntPtr l);
 public static string Go(){ var sb=new StringBuilder();
  EnumWindows((h,l)=>{ var c=new StringBuilder(256); GetClassName(h,c,256);
   if(c.ToString().StartsWith("Chibipop")) sb.AppendLine(c.ToString()); return true;},IntPtr.Zero);
  return sb.ToString(); } }
'@
[W]::Go()
Stop-Process -Name chibipop -Force
```

Expected: the popup, overlay and tray-owner classes. The settings class will **not** appear — it is only registered when the window opens, which needs a tray click. Record that as the known limit rather than as a failure.

- [ ] **Step 5: Commit**

```bash
git add src/ui/settings_window.rs src/ui/mod.rs src/ui/tray.rs src/app.rs
git commit -m "feat(ui): a modeless settings window, opened from the tray"
```

---

### Task 5: Populate and read back the controls

**Files:**
- Modify: `src/ui/settings_window.rs`

**Interfaces:**
- Consumes: `SettingsForm`, the three range constants.
- Produces: `SettingsWindow::read()` returning a `SettingsForm` reflecting the controls.

- [ ] **Step 1: Fill the font combo from installed Japanese-capable families**

```rust
/// Installed font families that can render Japanese, for the font combo.
///
/// Enumerated with `SHIFTJIS_CHARSET` so the list cannot offer a face that
/// would draw the popup's text as boxes. Families, not faces — weight and
/// style variants collapse to one entry, which matches what
/// `Theme::font_name` can express.
fn japanese_font_families() -> Vec<String> {
    // SAFETY: `lf` is stack storage valid for the call; `EnumFontFamiliesExW`
    // only reads it. The callback receives a pointer to our `Vec` as its
    // lparam and is invoked synchronously on this thread for the duration of
    // this call, so the reference cannot outlive the `Vec`.
    unsafe {
        let mut out: Vec<String> = Vec::new();
        let hdc = GetDC(None);
        let mut lf = LOGFONTW { lfCharSet: SHIFTJIS_CHARSET, ..Default::default() };
        EnumFontFamiliesExW(hdc, &mut lf, Some(enum_font_cb), LPARAM(&mut out as *mut _ as isize), 0);
        ReleaseDC(None, hdc);
        out.sort();
        out.dedup();
        out
    }
}
```

with a matching `unsafe extern "system" fn enum_font_cb` that pushes `lfFaceName` and returns 1. Skip names beginning `@` — those are the vertical-writing duplicates Windows lists alongside each family.

**The configured font is inserted and selected even when absent from the list** (spec D4): opening Settings and pressing Apply without touching anything must never change a setting.

- [ ] **Step 2: Populate every control from the form**

One `set_*` per control group, called from `open`. The spin controls are `msctls_updown32` with `UDM_SETRANGE32` set from `MAX_HEIGHT_RANGE`, `SUMMARY_RANGE` and `PASSES_RANGE`, buddied to a read-only `EDIT`.

- [ ] **Step 3: Implement `read()`**

Reads every control back into a fresh `SettingsForm`. `dict_names` comes from the listbox in its current order.

- [ ] **Step 4: Wire Move up / Move down**

Swap the selected item with its neighbour, **keep the selection on the moved item**, and enable/disable the buttons at the ends (spec D3). Nothing else in the window changes.

- [ ] **Step 5: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo build --release 2>&1 | grep -E "^error|Finished"
```

**Do not claim the controls were verified.** They cannot be clicked by an agent. State that in the report.

- [ ] **Step 6: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "feat(ui): populate and read back the settings controls"
```

---

### Task 6: Apply — persist, spawn, quit

**Files:**
- Modify: `src/app.rs`, `README.md`

**Interfaces:**
- Consumes: `SettingsWindow::take_outcome`, `SettingsWindow::read`, `settings::apply_to`.

- [ ] **Step 1: Handle the outcome in the loop**

In `run`'s `WM_TIMER` arm, after the existing work:

```rust
            if let Some(w) = &settings {
                match w.take_outcome() {
                    Some(SettingsOutcome::Cancel) => settings = None,
                    Some(SettingsOutcome::Apply) => {
                        let updated = settings::apply_to(&w.read(), &cfg);
                        if let Err(e) = updated.save(config_path) {
                            eprintln!("chibipop: could not save settings to {}: {e:#}",
                                      config_path.display());
                        } else if let Err(e) = restart_self() {
                            eprintln!("chibipop: could not restart: {e:#}");
                        } else {
                            unsafe { PostQuitMessage(0) };
                        }
                    }
                    None => {}
                }
            }
```

**Both failures leave the running instance alive and the window open.** A settings round-trip that half-applies — new file on disk, old settings running, or worse, nothing running at all — is the one outcome worth refusing outright.

- [ ] **Step 2: Write `restart_self`**

Beside `cursor_now` in `src/app.rs`:

```rust
/// Launch a replacement chibipop with this process's own argv.
///
/// Passing the original arguments is what makes `--dict`, `--rules` and
/// `--config` survive a settings change; a restart that silently reverted to
/// the defaults would be worse than not restarting.
///
/// The caller posts a quit **only after this returns `Ok`**, so a failure to
/// spawn leaves the working instance running rather than leaving the user
/// with nothing.
///
/// Startup measures 0.18s to a live tray icon, so the gap is imperceptible;
/// see the settings spec's D8.
fn restart_self() -> Result<()> {
    let exe = std::env::current_exe().context("locating this executable")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new(exe)
        .args(args)
        .spawn()
        .context("spawning the replacement process")?;
    Ok(())
}
```

`std::process::Command`, not `CreateProcessW` — it is in the standard library, it handles quoting, and this needs nothing Win32-specific.

- [ ] **Step 3: Verify, including that a restart actually works**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo build --release 2>&1 | grep -E "^error|Finished"
```

`restart_self` itself **can** be exercised without a UI: it is an ordinary function. Do not add a test that actually spawns chibipop from the suite — a test that leaves a tray icon behind on failure is worse than no test. Verify by reading, and note it.

- [ ] **Step 4: Document and commit**

In `README.md`'s Configuration section, add above the TOML block: right-clicking the tray icon offers **Settings…** and **Quit**; the window edits every setting below and Apply saves and restarts chibipop. Note that the TOML remains the reference and is still hand-editable.

```bash
git add src/app.rs README.md
git commit -m "feat(app): apply settings by saving and restarting"
```

---

### Task 7: Acceptance

**Files:**
- Create: `docs/superpowers/findings/2026-07-28-settings-gui-acceptance.md`

**This task measures. It does not tune.**

- [ ] **Step 1: Record what is provable**

Test totals, clippy state, release build, the exe size growth from Task 1, and the two load-bearing falsifications (Task 3 Step 6's substring rule; Task 1's link-arg growth).

- [ ] **Step 2: Record what is not, and why**

Synthetic input cannot reach the hook (`SendInput` returns 0), so the tray menu cannot be opened and no control can be clicked. **Every item in the spec's §2 acceptance list is the user's.** Say so plainly rather than implying the unit tests cover it.

- [ ] **Step 3: Write the manual script**

Reproduce the spec's §6 list verbatim as ten numbered items, each with what to do and what counts as pass. Items 7 and 8 (the TOML still holds the original substrings; a planted nonsense entry is reported) are the ones that catch D6/D6a and are invisible from the UI — mark them as such.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/findings/2026-07-28-settings-gui-acceptance.md
git commit -m "docs: settings GUI acceptance and the manual script"
```

---

## Self-Review

**Spec coverage:** §2.1 tray → Task 4 Step 1. §2.2 all eleven settings → Task 5 Steps 2–3 (table in D3). §2.3 Apply persists and restarts → Task 6. §2.4 dictionary reorder → Task 5 Step 4 + Task 3's write-back. §2.5 chibipop keeps working → Task 4 Step 3's `IsDialogMessageW` branch. D1 manifest/icon → Task 1. D2 modeless → Task 4 Steps 2–3. D3 controls/no free text/group box/listbox selection → Task 5. D4 font list → Task 5 Step 1. D5 prose labels and the tiling warning → Task 5 Step 2. D6 write-back → Task 3 Step 4 + falsification in Step 6. D6a stale entries → Task 3 (`stale_order_entries`) and Task 4 Step 2 item 7 (the label). D7 handshake → Task 2. D8 apply/spawn/quit → Task 6. D9 clone boundary → Task 3's module docs. §5's error table → Task 4 Step 3 (window creation), Task 6 Step 1 (save and spawn), Task 5 Step 1 (absent font), Task 3 (`stale_order_entries` keeps the entry). §6 testing → Task 3's tests and Task 7.

**Gap found and closed:** the spec's §5 requires `Settings…` on an already-open window to call `SetForegroundWindow` rather than opening a second. No step covered it; added to Task 4 Step 3's `OpenSettings` arm.

**Second gap found and closed:** §7 requires `MessageBoxW` in the error paths to disarm the wheel first, since it is modal and pumps. Task 6 Step 1 therefore uses `eprintln!` rather than a message box — which sidesteps the hazard entirely at the cost of a less visible error. Recorded here as a deliberate narrowing of the spec: if a message box is wanted later, `Hooks::set_scroll_armed(false)` must precede it.

**Placeholder scan:** Task 4 Step 2 and Task 5 describe control creation in groups rather than quoting every `CreateWindowExW` call, because eleven controls of five kinds is mechanical repetition and the file is built incrementally with a compile after each group. Every non-mechanical decision — the font enumeration, the ranges, the selection behaviour, the fallback — is quoted in full.

**Type consistency:** `SettingsForm` defined in Task 3 with eleven fields, consumed in Tasks 4–6. `from_config(&Config, &[DictInfo]) -> SettingsForm` and `apply_to(&SettingsForm, &Config) -> Config` are used exactly so in Task 4 Step 3 and Task 6 Step 1. `stale_order_entries(&Config, &[DictInfo]) -> Vec<String>` matches Task 4's `SettingsWindow::open(form, stale, font)` third parameter being `&str`. `TrayCommand::OpenSettings` defined in Task 4 Step 1, matched in Step 3. `SettingsOutcome` defined in Task 4 Step 2, matched in Task 6 Step 1. The three range constants are defined in Task 3 and used in Task 5 Step 2.
