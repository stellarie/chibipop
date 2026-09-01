#!/usr/bin/env python3
"""Run and record the full chibipop regression checklist.

This runner mirrors docs/REGRESSION.md. It automates the checks that are safe
from a command line and records an explicit result for every manual item.

No machine-specific paths are embedded here. Pass installs, archives, browser
commands, Anki data, and output locations as arguments.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import textwrap
import time
import ctypes
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable


STATUS_PASS = "PASS"
STATUS_FAIL = "FAIL"
STATUS_SKIP = "SKIP"
STATUS_XFAIL = "XFAIL"
STATUS_MANUAL = "MANUAL"

ROOT = Path(__file__).resolve().parents[1]


class HelpFormatter(
    argparse.ArgumentDefaultsHelpFormatter,
    argparse.RawDescriptionHelpFormatter,
):
    pass


@dataclass(frozen=True)
class Check:
    ident: str
    tier: str
    title: str
    mode: str
    doc_ref: str
    prompt: str
    destructive: bool = False
    known_gap: bool = False
    auto: str | None = None
    effects: tuple[str, ...] = ()


@dataclass
class Result:
    ident: str
    tier: str
    title: str
    mode: str
    status: str
    detail: str = ""
    seconds: float = 0.0
    evidence: dict[str, object] = field(default_factory=dict)
    cleanup: str = "not_applicable"


@dataclass
class Target:
    name: str
    exe: Path
    disposable: bool = False

    @property
    def root(self) -> Path:
        return self.exe.parent


def build_checks() -> list[Check]:
    checks = [
        Check("0.1", "0", "Rust tests, three times", "auto", "docs/REGRESSION.md#tier-0", "Run the Windows workspace test gate.", auto="cargo_tests"),
        Check("0.2", "0", "Clippy accepted finding count", "auto", "docs/REGRESSION.md#tier-0", "Count accepted clippy warnings exactly.", auto="clippy_accepted"),
        Check("0.3", "0", "Clippy with accepted lints suppressed", "auto", "docs/REGRESSION.md#tier-0", "Assert no other clippy errors or warnings.", auto="clippy_suppressed"),
        Check("0.4", "0", "Release build", "auto", "docs/REGRESSION.md#tier-0", "Build the release binary.", auto="release_build"),
        Check("0.5", "0", "Apply handler latency line", "interactive", "docs/REGRESSION.md#tier-0", "While running live Apply checks, record any 'Apply took N ms' stderr line."),
        Check("1.1", "1", "Pipeline resolves and looks up", "auto-or-interactive", "docs/REGRESSION.md#11-the-pipeline-resolves-and-looks-up", "Run probe at a known corpus point and require orient, line, at, anchor, ranked hits, and match.", auto="probe_pipeline"),
        Check("1.2", "1", "Match highlight geometry", "interactive", "docs/REGRESSION.md#12-the-match-highlight-is-where-it-claims", "Predict the glyph union, then verify the match rectangle equals it plus padding."),
        Check("1.3", "1", "Deconjugation highlight", "interactive", "docs/REGRESSION.md#13-the-deconjugation-case", "Hover the conjugated phrase and confirm the match box covers the full matched phrase."),
        Check("1.4", "1", "Probe region drawing", "auto-or-interactive", "docs/REGRESSION.md#14-draw-it-and-look", "Run probe with --show-region and inspect the capture and match boxes.", auto="probe_show_region"),
        Check("1.5", "1", "Same glyph stability", "auto-or-interactive", "docs/REGRESSION.md#15-same-glyph-stability-the-anti-flicker-precondition", "Probe 4 to 5 points inside one glyph and diff the hit lists.", auto="probe_stability"),
        Check("1.6", "1", "Vertical text known ceiling", "expected", "docs/REGRESSION.md#16-vertical-text-is-still-broken-in-the-known-way", "Confirm the wide region still fails or fabricates text, while the tall region reads the column.", known_gap=True, auto="probe_vertical"),
        Check("1.7", "1", "Wheel not swallowed at rest", "interactive", "docs/REGRESSION.md#17-the-wheel-is-not-swallowed-at-rest", "With run live and no popup, park over a scrollable window and wheel. The page must scroll."),
        Check("1.7a", "1", "Outlined glyph ceiling", "auto-or-interactive", "docs/REGRESSION.md#17a-outlined-glyphs-still-read-at-about-half", "Score outlined text from ocr line 0 and compare with solid text in the same run.", auto="probe_outlined"),
        Check("1.8", "1", "Resources", "auto-or-interactive", "docs/REGRESSION.md#18-resources", "Measure exe size, idle resources, watch plateau, startup time, and sustained hover memory.", auto="resources"),
        Check("1.9", "1", "Settings apply without restarting", "interactive", "docs/REGRESSION.md#19-settings-apply-without-restarting", "Change capture height, Apply, verify PID unchanged, window remains, clamp message, and probe height."),
        Check("1.10", "1", "Alphanumeric scanning", "interactive", "docs/REGRESSION.md#110-alphanumeric-scanning", "Disable alphanumeric scan live and check English, mixed numeric Japanese, and numeric hover behavior."),
        Check("1.11", "1", "Trigger and hotkeys apply live", "interactive", "docs/REGRESSION.md#111-trigger-mode-and-both-hotkeys-apply-live", "Change trigger mode, trigger key, and Anki key. Each must work with the same PID."),
        Check("1.11.1", "1", "Trigger mode switches live", "interactive", "docs/REGRESSION.md#111-trigger-mode-and-both-hotkeys-apply-live", "Switch Trigger from Live to Hold key, Apply, and confirm hover alone stops while hold-key lookup works."),
        Check("1.11.2", "1", "Trigger key changes live", "interactive", "docs/REGRESSION.md#111-trigger-mode-and-both-hotkeys-apply-live", "Capture a different trigger key, Apply, and confirm the new key works while the old one does not."),
        Check("1.11.3", "1", "Anki shortcut changes live", "destructive", "docs/REGRESSION.md#111-trigger-mode-and-both-hotkeys-apply-live", "Change the Anki shortcut key, Apply, and add a scratch card with the new key.", destructive=True),
        Check("1.12", "1", "Scan overlay live toggle", "interactive", "docs/REGRESSION.md#112-the-scan-overlay-can-be-switched-on-live", "Start with scan outline off, enable it, Apply, hover, and see capture boxes without restart."),
        Check("1.13", "1", "Capture exclusion live toggle", "interactive", "docs/REGRESSION.md#113-the-capture-guard-tracks-a-live-exclude_from_capture-toggle", "Toggle capture exclusion off and on. Popup text must not contaminate OCR."),
        Check("1.14", "1", "Per-character retrigger", "interactive", "docs/REGRESSION.md#114-per-character-retrigger", "Run toggle-on, toggle-off, already-visible-popup, hold-key disabled, vertical, wheel, and drill-down checks."),
        Check("1.14.1", "1", "Horizontal per-character toggle", "interactive", "docs/REGRESSION.md#114-per-character-retrigger", "With Live trigger and the checkbox on, hover the next character in a word. It must retrigger; with it off, it must hold the word."),
        Check("1.14.2", "1", "Popup reach behavior during retrigger", "interactive", "docs/REGRESSION.md#114-per-character-retrigger", "In both toggle states, move into the popup and verify hold, wheel scroll, and kanji drill-down still work."),
        Check("1.14.3", "1", "Hold-key mode disables checkbox", "interactive", "docs/REGRESSION.md#114-per-character-retrigger", "Switch Trigger to Hold key and confirm the per-character checkbox greys out and stays inert."),
        Check("1.14.4", "1", "Per-character Apply stays in-process", "interactive", "docs/REGRESSION.md#114-per-character-retrigger", "Confirm the PID does not change, and that an already-visible popup observes the setting immediately after Apply."),
        Check("1.14.5", "1", "Vertical retrigger ceiling", "expected", "docs/REGRESSION.md#114-per-character-retrigger", "In vertical text, moving down the reading axis is expected not to retrigger. Record that as expected behavior.", known_gap=True),
        Check("1.15", "1", "OCR language live Apply", "interactive", "docs/REGRESSION.md#115-ocr-language", "Switch OCR language and back with the PID unchanged, and verify Japanese stops then returns."),
        Check("1.15.1", "1", "Language switch negative and return", "interactive", "docs/REGRESSION.md#115-ocr-language", "Switch OCR language, Apply, verify Japanese stops with the same PID, then switch back and verify it returns."),
        Check("1.15.2", "1", "Language dropdown contents", "interactive", "docs/REGRESSION.md#115-ocr-language", "Confirm the dropdown lists installed recognizers and appends a configured missing tag as '<tag> (not installed)'."),
        Check("1.15.3", "1", "BCP-47 tag self-healing", "interactive", "docs/REGRESSION.md#115-ocr-language", "Apply a matching shorthand tag and confirm the TOML rewrites to the installed Windows tag, not a different language."),
        Check("1.15.4", "1", "Missing pack keeps prior engine", "interactive", "docs/REGRESSION.md#115-ocr-language", "Remove or simulate a missing selected pack and confirm lookups keep working with the previous recognizer."),
        Check("1.15.5", "1", "Reload stderr classification", "interactive", "docs/REGRESSION.md#115-ocr-language", "Read stderr and classify the reload message as missing-pack, recognizer-build-failed, or expected silence."),
        Check("1.16", "1", "Startup language fallback", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Configure a missing OCR pack, start from a terminal, read stderr, verify fallback lookups, then restore config."),
        Check("1.16.1", "1", "Startup fallback preflight stopped", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Quit chibipop and confirm no target process remains before editing the OCR language."),
        Check("1.16.2", "1", "Startup fallback missing tag", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Set ocr.language to a tag with no installed pack and confirm it is absent first."),
        Check("1.16.3", "1", "Startup fallback terminal launch", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Launch run from a terminal and record the startup substitution warning on stderr."),
        Check("1.16.4", "1", "Startup fallback lookup", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Confirm Japanese lookup works after startup falls back to an installed recognizer."),
        Check("1.16.5", "1", "Startup fallback restore", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Restore the OCR language in Settings or in the config file."),
        Check("1.16.6", "1", "Startup fallback settings display", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Confirm Settings shows the configured missing tag while OCR runs the fallback language."),
        Check("1.16.7", "1", "Startup fallback dictionary scope", "interactive", "docs/REGRESSION.md#116-the-startup-language-fallback", "Confirm a missing-pack per-language list is ignored and the fallback language searches every dictionary."),
        Check("1.17", "1", "Per-language dictionary lists", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Run the full two-list language-scope workflow and confirm runtime lookup scope."),
        Check("1.17.1", "1", "Dictionary list boundary movement", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Move rows within and across Searched and Not searched only at boundaries."),
        Check("1.17.2", "1", "Dictionary list acting box", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Select in Not searched and ensure Move and Remove act on that box."),
        Check("1.17.3", "1", "Last searched dictionary guard", "destructive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Try to move or remove the last readable searched dictionary. It must remain scoped.", destructive=True),
        Check("1.17.4", "1", "Dictionary Add appends to Searched", "destructive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Import a dictionary and confirm it lands at the bottom of Searched.", destructive=True),
        Check("1.17.5", "1", "Stale dictionary list fallback", "destructive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Hand-edit stale per-language entries and confirm both routes search everything.", destructive=True),
        Check("1.17.6", "1", "Dictionary tab two-box shape", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Confirm the tab has Searched and Not searched listboxes, four buttons, and no divider row."),
        Check("1.17.7", "1", "Per-language Apply stays in-process", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Confirm the PID does not change across Apply."),
        Check("1.17.8", "1", "Language dropdown rescopes lists", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Switch OCR language and confirm the dictionary boxes rescope before Apply."),
        Check("1.17.9", "1", "Runtime lookup scope changes", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Hover the same word after switching language scope and confirm a different dictionary set answers."),
        Check("1.17.10", "1", "Unsaved language edits survive switching", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Edit one language's list, switch away and back without Apply, and confirm the edit is still present."),
        Check("1.17.11", "1", "Unscoped language searches all", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Select a language with no list and confirm every dictionary is searched."),
        Check("1.17.12", "1", "Per-language TOML keys use stable substrings", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "After Apply, confirm per-language dictionary names are cut before '[' or '(' when applicable."),
        Check("1.17.13", "1", "Dictionary tab layout at height limits", "interactive", "docs/REGRESSION.md#117-per-language-dictionary-lists", "Confirm four full rows per listbox, scrollbar on a fifth row, unclipped captions, one-line hint, and visible Apply/Quit controls."),
        Check("1.18", "1", "Incremental dictionary Apply", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Run all fifteen incremental add, remove, replace, latency, responsiveness, and frequency-refusal checks.", destructive=True),
        Check("1.18.1", "1", "Live run PID and baseline hover", "interactive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Start run from a terminal, record PID and confirm baseline popup."),
        Check("1.18.2", "1", "Dictionary Apply wording", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Before Apply, confirm the hint says updates in place and the button says Apply.", destructive=True),
        Check("1.18.3", "1", "Dictionary Add progress", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Confirm Reading, rebased entry counts, Added message, and seconds-scale completion.", destructive=True),
        Check("1.18.4", "1", "Dictionary Apply no restart", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Confirm PID unchanged and no window flicker.", destructive=True),
        Check("1.18.5", "1", "New dictionary answers immediately", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Hover text covered only by the new dictionary and confirm it answers without reopen.", destructive=True),
        Check("1.18.6", "1", "Popup stays live during Apply", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Hover repeatedly during the import and confirm old rows still answer.", destructive=True),
        Check("1.18.7", "1", "Removed dictionary stops immediately", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Remove the dictionary and confirm it stops answering without restart.", destructive=True),
        Check("1.18.8", "1", "Remove and add in one Apply", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Replace one dictionary with another in one Apply and verify both effects.", destructive=True),
        Check("1.18.9", "1", "Dictionary plus settings Apply", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Stage a dictionary import and a non-dictionary setting. Both must land.", destructive=True),
        Check("1.18.10", "1", "Post-Apply filesystem state", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Confirm .removed is gone, archive locations are correct, DB mtime changed, and no .new exists.", destructive=True),
        Check("1.18.11", "1", "Apply latency report", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Report the Apply took line if present, or record that no line appeared.", destructive=True),
        Check("1.18.12", "1", "Tray quit after Apply", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Quit from tray after Apply and confirm exit within about one second.", destructive=True),
        Check("1.18.13", "1", "Hide acknowledgement stderr", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Read stderr for 'hide was not acknowledged' and classify one in-flight line as expected.", destructive=True),
        Check("1.18.14", "1", "Desktop responsiveness", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Move mouse and type through Apply and quit. Desktop must not lag or hover must not die.", destructive=True),
        Check("1.18.15", "1", "Frequency refusal", "destructive", "docs/REGRESSION.md#118-a-dictionary-change-lands-in-seconds-without-a-restart", "Stage a frequency archive and confirm refusal, preserved files, and readable CRLF command.", destructive=True),
        Check("1.19", "1", "Library/database drift notice", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Create both drift directions and no-notice cases. Confirm no automatic rebuild.", destructive=True),
        Check("1.19.1", "1", "Drift notice extra library archive", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "With chibipop stopped, drop a term archive directly into library instead of using Add.", destructive=True),
        Check("1.19.2", "1", "Drift notice in settings", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Start run, open Settings, and confirm the status names the extra library archive and command.", destructive=True),
        Check("1.19.3", "1", "Drift notice does not rebuild", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Confirm there is no rebuild, busy window, child process, or database mtime change.", destructive=True),
        Check("1.19.4", "1", "Drift notice missing library archive", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Move the archive back out and confirm Settings names it as in the database but absent from library.", destructive=True),
        Check("1.19.5", "1", "Drift checked from both settings routes", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Open Settings from startup and from the tray and confirm both routes check drift.", destructive=True),
        Check("1.19.6", "1", "Drift false alarms stay silent", "destructive", "docs/REGRESSION.md#119-the-database-can-now-drift-from-the-library-and-says-so", "Confirm no notice for no term archive, corrupt archive, or absent/unparseable source_hashes.", destructive=True),
        Check("1.20", "1", "Standalone settings rebuild failure", "destructive", "docs/REGRESSION.md#120-chibipop-settings-still-rebuilds-and-now-fails-generically", "Apply a dictionary change from settings while run holds the DB and confirm rollback.", destructive=True),
        Check("1.20.1", "1", "Standalone settings rollback evidence", "destructive", "docs/REGRESSION.md#120-chibipop-settings-still-rebuilds-and-now-fails-generically", "Confirm generic rebuild failure text, restored archives, empty or missing .removed, old database mtime, and live hovers.", destructive=True),
        Check("1.20.2", "1", "Standalone settings live hover proof", "destructive", "docs/REGRESSION.md#120-chibipop-settings-still-rebuilds-and-now-fails-generically", "After rollback, confirm hovers in the live instance still answer from the unchanged database.", destructive=True),
        Check("1.21", "1", "All OCR languages resolve", "interactive", "docs/REGRESSION.md#121-all-three-ocr-languages-resolve", "Test ja, zh-Hans-CN, and zh-Hant-TW through real run with matching dictionaries and recognizers."),
        Check("1.21.1", "1", "Japanese OCR language resolves", "interactive", "docs/REGRESSION.md#121-all-three-ocr-languages-resolve", "Run with ocr.language=ja and confirm the J1 fixture resolves the documented Japanese word and dictionaries."),
        Check("1.21.2", "1", "Simplified Chinese OCR language resolves", "interactive", "docs/REGRESSION.md#121-all-three-ocr-languages-resolve", "Run with ocr.language=zh-Hans-CN and confirm the Simplified Chinese fixture resolves with Chinese dictionaries only."),
        Check("1.21.3", "1", "Traditional Chinese OCR language resolves", "interactive", "docs/REGRESSION.md#121-all-three-ocr-languages-resolve", "Run with ocr.language=zh-Hant-TW and confirm the Traditional Chinese fixture resolves with Chinese dictionaries only."),
        Check("1.21.4", "1", "Installed OCR recognizer preflight", "interactive", "docs/REGRESSION.md#121-all-three-ocr-languages-resolve", "List installed Windows OCR recognizers with Windows PowerShell 5.1 and classify missing packs as environment gaps."),
        Check("1.22", "1", "Anki HTML card", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Mine scratch Anki notes for all languages, verify glossary_html, duplicates, and cleanup.", destructive=True),
        Check("1.22.1", "1", "Japanese Anki HTML cards", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Mine the documented Japanese words into a scratch deck and verify glossary_html lengths and frequency.", destructive=True),
        Check("1.22.2", "1", "Simplified Chinese Anki HTML cards", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Mine Simplified Chinese words into a scratch deck and verify glossary_html output and no Japanese frequency.", destructive=True),
        Check("1.22.3", "1", "Traditional Chinese Anki HTML cards", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Mine Traditional Chinese words into a scratch deck and verify glossary_html output.", destructive=True),
        Check("1.22.4", "1", "Anki deconjugation and vertical sources", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Confirm a deconjugated word and a vertical-column word mine to their dictionary forms.", destructive=True),
        Check("1.22.5", "1", "Anki HTML structure", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Verify real HTML structure such as lists, tables, spans, and cross-reference anchors.", destructive=True),
        Check("1.22.6", "1", "Anki duplicate guard", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Add the same expression twice and confirm the duplicate failure appears as expected.", destructive=True),
        Check("1.22.7", "1", "Anki scratch cleanup", "destructive", "docs/REGRESSION.md#122-the-anki-card-carries-html-if-the-field-map-asks-for-it", "Delete scratch notes and decks, then confirm deckNames returned to the prior state.", destructive=True),
        Check("1.23", "1", "Real text PNG encode bracket", "interactive", "docs/REGRESSION.md#123-real-text-inside-the-png-encode-bracket", "Capture real J1 pixels, encode through encode_png, and record bytes plus p95 under 10 ms."),
        Check("1.23.1", "1", "PNG encode real corpus setup", "interactive", "docs/REGRESSION.md#123-real-text-inside-the-png-encode-bracket", "Put the OCR corpus full-screen at the expected size or record the actual viewport."),
        Check("1.23.2", "1", "PNG encode real capture", "interactive", "docs/REGRESSION.md#123-real-text-inside-the-png-encode-bracket", "Capture the JA J1 region with probe --dump and record the real buffer."),
        Check("1.23.3", "1", "PNG encode p95", "interactive", "docs/REGRESSION.md#123-real-text-inside-the-png-encode-bracket", "Encode the real buffer through encode_png and record byte count plus p95 under 10 ms."),
        Check("1.24", "1", "Provider trait no behavior change", "interactive", "docs/REGRESSION.md#124-provider-trait-no-behaviour-change", "Hover J1 and compare popup text, resolved word, and highlight rect with the baseline."),
        Check("1.25", "1", "Plugin CLI exit codes", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Create echo and broken fixture manifests beside the target exe, run five CLI cases, then clean up.", auto="plugin_cli"),
        Check("1.25.1", "1", "Plugin list exit code", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Run plugin list and confirm broken is refused, echo is clean, and exit code is 1."),
        Check("1.25.2", "1", "Plugin echo test exit code", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Run plugin test echo with the sample PNG and confirm exit code 0."),
        Check("1.25.3", "1", "Plugin broken test exit code", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Run plugin test broken with the sample PNG and confirm exit code 1."),
        Check("1.25.4", "1", "Plugin unknown-name exit code", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Run plugin test nosuchplugin with the sample PNG and confirm exit code 2."),
        Check("1.25.5", "1", "Plugin missing-image exit code", "auto-or-interactive", "docs/REGRESSION.md#125-chibipop-plugin-cli-exit-codes", "Run plugin test echo with a missing image and confirm exit code 2."),
        Check("1.26", "1", "Scrollable settings window", "auto-or-interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Run settings --audit and the seven visible scroll checks.", auto="settings_audit"),
        Check("1.26.1", "1", "Settings Apply row same y", "auto-or-interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Use settings --audit and a shrunk live window to confirm Apply has the same y on every tab.", auto="settings_audit"),
        Check("1.26.2", "1", "Settings scrollbar appears only when needed", "interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Confirm tall tabs get a scrollbar only when the viewport is shrunk below content height."),
        Check("1.26.3", "1", "Settings wheel clamp", "interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Wheel a tall tab to both ends and confirm it clamps without drift."),
        Check("1.26.4", "1", "Settings tab switch resets scroll", "interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Scroll a tab, switch away and back, and confirm it returns to the top."),
        Check("1.26.5", "1", "Settings controls still respond", "auto-or-interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Operate visible controls on each tab and compare settings --audit tab rings when useful."),
        Check("1.26.6", "1", "Settings scrollbar thumb drag", "interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "Drag the scrollbar thumb by hand and confirm content tracks smoothly and stays where released."),
        Check("1.26.7", "1", "Settings 150 percent scaling", "interactive", "docs/REGRESSION.md#126-the-scrollable-settings-window", "At 150 percent display scaling, restart and confirm Apply is visible or reachable on every tab."),
        Check("1.27", "1", "Live Apply engine switching", "expected", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Record implemented parts and expected blocked hot-swap and notice parts.", known_gap=True),
        Check("1.27.1", "1", "Plugin enable transition", "interactive", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Enable an available plugin and confirm status reaches Ready without crashing."),
        Check("1.27.2", "1", "Plugin engine selection transition", "expected", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Select the plugin as engine, Apply, and record whether the next hover uses it or the known hot-swap gap blocks it.", known_gap=True),
        Check("1.27.3", "1", "Built-in engine reselect transition", "interactive", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Select Built-in again, Apply, and confirm the same hover uses Windows OCR."),
        Check("1.27.4", "1", "Plugin failure auto-disable transition", "expected", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Exhaust a failing plugin and record the current silent failure or future auto-revert notice behavior.", known_gap=True),
        Check("1.27.5", "1", "Three-failure notice transition", "expected", "docs/REGRESSION.md#127-live-apply-engine-switching-transitions", "Trigger three plugin failures outside Apply and record the current missing notice or future one-shot notice behavior.", known_gap=True),
        Check("1.28", "1", "Fresh install with discovered meikiocr", "interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Seed a scratch install and confirm meikiocr discovery without premature adapter start."),
        Check("1.28.1", "1", "Fresh meikiocr startup", "interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Run a scratch install and confirm startup has no plugin warnings and reports windows-ocr."),
        Check("1.28.2", "1", "Fresh meikiocr tabs", "auto-or-interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Open settings and confirm the five tabs are present.", auto="fresh_meikiocr_audit"),
        Check("1.28.3", "1", "Fresh meikiocr engine dropdown", "auto-or-interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Confirm the OCR engine dropdown lists Built-in and meikiocr.", auto="fresh_meikiocr_audit"),
        Check("1.28.4", "1", "Fresh meikiocr plugin tab", "auto-or-interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Confirm the Plugins tab lists meikiocr as Enabled and does not show No plugins found.", auto="fresh_meikiocr_audit"),
        Check("1.28.5", "1", "Fresh meikiocr builtin OCR", "interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Hover Japanese text and confirm it resolves through the built-in engine."),
        Check("1.28.6", "1", "Fresh meikiocr no premature adapter", "interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Confirm no meikiocr adapter line appears on stderr before selecting the plugin engine."),
        Check("1.28.7", "1", "Fresh meikiocr config persistence", "interactive", "docs/REGRESSION.md#128-fresh-install-with-discovered-meikiocr", "Apply the checked plugin row, reopen settings, and confirm saved and discovery-extended states behave the same."),
        Check("1.29", "1", "Per-engine live regression", "interactive", "docs/REGRESSION.md#129-per-engine-live-regression", "Restart for builtin, meikiocr, and nonexistent engine fallback. Record lines and resolved words."),
        Check("1.29.1", "1", "Built-in engine live pass", "interactive", "docs/REGRESSION.md#129-per-engine-live-regression", "Restart with builtin selected, hover m26, and record the resolved word plus windows-ocr startup line."),
        Check("1.29.2", "1", "Meikiocr engine live pass", "interactive", "docs/REGRESSION.md#129-per-engine-live-regression", "Restart with meikiocr selected, hover m26, and record the resolved word plus adapter stderr lines."),
        Check("1.29.3", "1", "Per-engine comparison", "interactive", "docs/REGRESSION.md#129-per-engine-live-regression", "Compare built-in and meikiocr resolved words at the same coordinates and record any difference as a finding."),
        Check("1.29.4", "1", "Unknown engine fallback", "interactive", "docs/REGRESSION.md#129-per-engine-live-regression", "Restart with an unknown engine name and confirm the exact fallback warning, windows-ocr startup, and normal hover."),
        Check("1.30", "1", "Screenshot action", "destructive", "docs/REGRESSION.md#130-screenshot-action", "Run all screenshot selection, cancel, Anki image, no-popup, and hot-reload checks.", destructive=True),
        Check("1.30.1", "1", "Screenshot overlay starts", "interactive", "docs/REGRESSION.md#130-screenshot-action", "With a popup visible, press the screenshot hotkey and confirm the dim overlay plus crosshair appears."),
        Check("1.30.2", "1", "Screenshot selection visuals", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Drag a region and confirm the selected area is undimmed with a white border."),
        Check("1.30.3", "1", "Screenshot PNG saved", "destructive", "docs/REGRESSION.md#130-screenshot-action", "Release selection and confirm a valid screenshot PNG is saved beside the target executable.", destructive=True),
        Check("1.30.4", "1", "Screenshot Anki image card", "destructive", "docs/REGRESSION.md#130-screenshot-action", "If Anki is connected, verify the scratch card contains an image tag and the media file exists.", destructive=True),
        Check("1.30.5", "1", "Screenshot Esc cancel", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Press Esc during selection and confirm the overlay closes, popup returns, and no file is saved."),
        Check("1.30.6", "1", "Screenshot right-click cancel", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Right-click during selection and confirm the same cancel behavior as Esc."),
        Check("1.30.7", "1", "Screenshot tiny-drag cancel", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Click or drag less than five pixels and confirm it is treated as cancel."),
        Check("1.30.8", "1", "Screenshot no-popup inert", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Trigger the screenshot hotkey with no popup visible and confirm it is silently ignored."),
        Check("1.30.9", "1", "Screenshot added-state duplicate", "destructive", "docs/REGRESSION.md#130-screenshot-action", "After a screenshot card, confirm the popup shows added and regular Anki add hits allowDuplicate=false.", destructive=True),
        Check("1.30.10", "1", "Screenshot hot reload", "interactive", "docs/REGRESSION.md#130-screenshot-action", "Change actions.screenshot.hotkey, Apply, and confirm the new hotkey works with the same PID."),
        Check("2.1", "2", "Hover popup appears", "interactive", "docs/REGRESSION.md#tier-2", "Hover Japanese text and confirm the popup appears beside it."),
        Check("2.2", "2", "Reach into popup", "interactive", "docs/REGRESSION.md#tier-2", "Move from word into popup. It must not change or vanish."),
        Check("2.3", "2", "Leave popup", "interactive", "docs/REGRESSION.md#tier-2", "Leave the popup and confirm normal hover resumes with no dead patch."),
        Check("2.4", "2", "Jiggle no flicker", "interactive", "docs/REGRESSION.md#tier-2", "Jiggle on one word and confirm no flicker."),
        Check("2.5", "2", "Scan sideways", "interactive", "docs/REGRESSION.md#tier-2", "Scan to the next word and confirm the next word resolves."),
        Check("2.6", "2", "Conjugated verb one popup", "interactive", "docs/REGRESSION.md#tier-2", "Scan along the conjugated verb and confirm one popup, not one per character."),
        Check("2.7", "2", "Overflowing entry scroll", "interactive", "docs/REGRESSION.md#tier-2", "Open an overflowing entry and wheel it end to end. Thumb ends flush."),
        Check("2.8", "2", "Underlying page wheels", "interactive", "docs/REGRESSION.md#tier-2", "Hover a word, do not move, wheel. The page underneath must scroll."),
        Check("2.9", "2", "Tray menu plus wheel", "expected", "docs/REGRESSION.md#tier-2", "Open the tray menu and wheel. Wheel must still work.", known_gap=True),
        Check("2.10", "2", "Quit then wheel", "interactive", "docs/REGRESSION.md#tier-2", "Quit chibipop, then wheel another app. Wheel must still work."),
        Check("2.11", "2", "Startup settings window", "interactive", "docs/REGRESSION.md#tier-2", "Start run with settings opening, verify buttons, TOML values, native controls, Cancel, and hover underneath."),
        Check("2.11a", "2", "Tray right click still no menu", "expected", "docs/REGRESSION.md#tier-2", "Right-click the tray icon. Known broken path should still show no menu.", known_gap=True),
        Check("2.11b", "2", "Apply caption and hint", "interactive", "docs/REGRESSION.md#tier-2", "Verify run and settings caption/hint table, including dictionary-staged wording."),
        Check("2.11c", "2", "Quit button exits", "auto-or-interactive", "docs/REGRESSION.md#tier-2", "Press Quit chibipop in settings and confirm the process exits.", auto="settings_desktop_smoke"),
        Check("2.11d", "2", "Double-click console hidden", "interactive", "docs/REGRESSION.md#tier-2", "Launch without inheriting a console and confirm ConsoleWindowClass is hidden."),
        Check("2.11e", "2", "WM_CLOSE behavior", "auto-or-interactive", "docs/REGRESSION.md#tier-2", "Close settings via X in standalone and normal run. Confirm each documented behavior.", auto="settings_desktop_smoke"),
        Check("2.12", "2", "Reorder dictionaries", "destructive", "docs/REGRESSION.md#tier-2", "Reorder dictionaries, Apply, and verify TOML substrings were reordered only.", destructive=True),
        Check("2.13", "2", "No-op Apply", "interactive", "docs/REGRESSION.md#tier-2", "Open Settings, touch nothing, Apply. TOML only formats and returns quickly."),
        Check("2.14", "2", "Settings Add archives", "destructive", "docs/REGRESSION.md#tier-2", "Import two term archives and one frequency archive, Apply, verify lookup changes, remove one, verify removal.", destructive=True),
        Check("2.14a", "2", "Settings Remove one", "destructive", "docs/REGRESSION.md#tier-2", "Remove one archive and Apply. Confirm list, status, DB dict table, and cleanup.", destructive=True),
        Check("2.14b", "2", "Remove everything refused", "destructive", "docs/REGRESSION.md#tier-2", "Remove everything and Apply. Confirm refusal, open window, unchanged DB hash, and no deleted archives.", destructive=True),
        Check("2.14c", "2", "Corrupt archive rollback", "destructive", "docs/REGRESSION.md#tier-2", "Put corrupt zip in library and Apply. Confirm generic failure and rollback over three presses.", destructive=True),
        Check("2.14d", "2", "Last real dictionary guard with bad archive", "destructive", "docs/REGRESSION.md#tier-2", "Remove last real dictionary with corrupt or frequency archive present. Confirm refusal.", destructive=True),
        Check("2.14e", "2", "Concurrent settings Apply lock", "destructive", "docs/REGRESSION.md#tier-2", "Open two settings windows and Apply different removals. One must be refused by the library lock.", destructive=True),
        Check("2.14f", "2", "Settings Apply while run holds DB", "destructive", "docs/REGRESSION.md#tier-2", "Apply from settings while run holds the DB. Confirm running-instance refusal and preserved archives.", destructive=True),
    ]
    return checks


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run and record the full chibipop regression checklist.",
        epilog=textwrap.dedent(
            """\
            Examples:
              python scripts/manual_regression.py --list
              python scripts/manual_regression.py --tier 0 --repo-root . --repeat-tests 3
              python scripts/manual_regression.py --test-install --tier 1 --allow-destructive
              python scripts/manual_regression.py --non-interactive --exe ./target/release/chibipop.exe --only 1.8
              python scripts/manual_regression.py --exe ./nightly/chibipop.exe --secondary-exe ./nightly-jp/chibipop.exe --corpus ./docs/fixtures/ocr-corpus.html
            """
        ),
        formatter_class=HelpFormatter,
    )
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--exe", type=Path, help="Primary chibipop executable or install directory.")
    parser.add_argument("--secondary-exe", action="append", default=[], type=Path, help="Secondary executable or install directory. Repeat for language-specific installs.")
    parser.add_argument("--target", action="append", default=[], metavar="NAME=PATH", help="Named target exe or install directory. Repeat for named installs.")
    parser.add_argument("--tier", choices=["all", "0", "1", "2"], default="all")
    parser.add_argument("--only", action="append", default=[], help="Run only matching check ids or prefixes. Repeatable.")
    parser.add_argument("--skip", action="append", default=[], help="Skip matching check ids or prefixes. Repeatable.")
    parser.add_argument("--list", action="store_true", help="List checks and exit.")
    parser.add_argument("--artifacts-dir", type=Path, default=Path("regression-artifacts"))
    parser.add_argument("--report", type=Path, help="JSON report path. Defaults inside --artifacts-dir.")
    parser.add_argument("--logs-dir", type=Path, help="Command log directory. Defaults inside --artifacts-dir.")
    parser.add_argument("--interactive", dest="interactive", action="store_true", default=sys.stdin.isatty())
    parser.add_argument("--no-interactive", dest="interactive", action="store_false")
    parser.add_argument("--non-interactive", dest="interactive", action="store_false")
    parser.add_argument("--strict", action="store_true", help="Fail when any check is skipped or left manual.")
    parser.add_argument("--allow-destructive", action="store_true", help="Allow checks that may mutate install state.")
    parser.add_argument("--allow-config-write", action="store_true", help="Allow checks that write chibipop configuration.")
    parser.add_argument("--allow-dictionary-mutation", action="store_true", help="Allow checks that add, remove, rebuild, or reorder dictionaries.")
    parser.add_argument("--allow-anki-write", action="store_true", help="Allow checks that create or modify Anki notes.")
    parser.add_argument("--allow-display-change", action="store_true", help="Allow checks that change display scaling.")
    parser.add_argument("--allow-real-target-destructive", action="store_true", help="Allow destructive checks against non-disposable target roots.")
    parser.add_argument("--keep-mutated-state", action="store_true", help="Do not restore protected state after destructive checks.")
    parser.add_argument("--allow-plugin-fixtures", action="store_true", help="Allow creating temporary echo and broken plugin fixture directories beside the selected target exe.")
    parser.add_argument("--stop-target-strays", action="store_true", help="Stop chibipop.exe processes only when their executable path contains this repo's target directory.")
    parser.add_argument("--test-install", action="store_true", help="Build release and seed a disposable install under --test-install-dir.")
    parser.add_argument("--test-install-dir", type=Path, default=Path(".scratch/regression-test-install"), help="Disposable install directory, relative to --repo-root by default.")
    parser.add_argument("--keep-test-install", action="store_true", help="Keep the disposable install after the run.")
    parser.add_argument("--repeat-tests", type=int, default=3)
    parser.add_argument("--min-test-total", type=int, default=400)
    parser.add_argument("--expected-clippy-warnings", type=int, default=1)
    parser.add_argument("--expected-other-clippy", type=int, default=0)
    parser.add_argument("--allow-local-golden-failure", action="store_true")
    parser.add_argument("--probe-point", action="append", default=[], metavar="NAME=X,Y", help="Named point for probe checks. Names: pipeline, highlight, deconj, draw, vertical, outlined, solid.")
    parser.add_argument("--region", default="", help="Default capture region for guided checks, as WIDTH,HEIGHT.")
    parser.add_argument("--ja-point", default="", help="Convenience alias for --probe-point pipeline=X,Y.")
    parser.add_argument("--zh-simplified-point", default="", help="Point for Simplified Chinese live checks.")
    parser.add_argument("--zh-traditional-point", default="", help="Point for Traditional Chinese live checks.")
    parser.add_argument("--alnum-point", default="", help="Point for alphanumeric live checks.")
    parser.add_argument("--vertical-point", default="", help="Convenience alias for --probe-point vertical=X,Y.")
    parser.add_argument("--show-region-seconds", type=int, default=8)
    parser.add_argument("--open-fixtures", action="store_true")
    parser.add_argument("--browser-command", action="append", default=[], help="Browser command token. Repeat for each token. Use {url} as the fixture URL placeholder.")
    parser.add_argument("--browser-cmd-template", help="Browser command template. Use {url} as the fixture URL placeholder.")
    parser.add_argument("--corpus", type=Path, help="OCR corpus HTML fixture. Defaults to docs/fixtures/ocr-corpus.html under --repo-root.")
    parser.add_argument("--scroll-fixture", type=Path, help="Scroll HTML fixture. Defaults to docs/fixtures/scroll-test.html under --repo-root.")
    parser.add_argument("--plugin-image", type=Path, help="Image fixture for plugin CLI tests.")
    parser.add_argument("--dictionary-archive", action="append", default=[], type=Path, help="Term dictionary archive. Repeatable.")
    parser.add_argument("--term-archive", action="append", default=[], type=Path, help="Alias for --dictionary-archive.")
    parser.add_argument("--frequency-archive", action="append", default=[], type=Path)
    parser.add_argument("--corrupt-archive", type=Path)
    parser.add_argument("--primary-language", default="", help="Language tag expected for the primary install.")
    parser.add_argument("--secondary-language", action="append", default=[], help="Language tag expected for a secondary install.")
    parser.add_argument("--anki-deck", default="")
    return parser.parse_args()


def command_text(cmd: list[str | os.PathLike[str]]) -> str:
    return " ".join(str(part) for part in cmd)


def run_cmd(cmd: list[str | os.PathLike[str]], cwd: Path, logs_dir: Path, name: str, timeout: int | None = None) -> tuple[int, str, float, Path]:
    logs_dir.mkdir(parents=True, exist_ok=True)
    log_path = logs_dir / f"{safe_name(name)}.log"
    start = time.perf_counter()
    try:
        proc = subprocess.run(
            [str(part) for part in cmd],
            cwd=str(cwd),
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
        )
        elapsed = time.perf_counter() - start
        output = proc.stdout
        code = proc.returncode
    except subprocess.TimeoutExpired as exc:
        elapsed = time.perf_counter() - start
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode("utf-8", errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode("utf-8", errors="replace")
        output = str(stdout) + str(stderr)
        output += f"\ncommand timed out after {timeout} seconds\n"
        code = 124
    log_path.write_text(output, encoding="utf-8", errors="replace")
    return code, output, elapsed, log_path


def safe_name(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", text).strip("_") or "log"


def parse_target(spec: str, repo_root: Path) -> Target:
    if "=" not in spec:
        raise ValueError(f"--target must be NAME=PATH, got {spec!r}")
    name, raw = spec.split("=", 1)
    if not name:
        raise ValueError("--target name cannot be empty")
    path = Path(raw)
    if not path.is_absolute():
        path = (repo_root / path).resolve()
    if path.is_dir():
        exe = path / ("chibipop.exe" if os.name == "nt" else "chibipop")
    else:
        exe = path
    return Target(name=name, exe=exe)


def default_target(repo_root: Path) -> Target:
    exe = repo_root / "target" / "release" / ("chibipop.exe" if os.name == "nt" else "chibipop")
    return Target("release", exe)


TEST_INSTALL_MARKER = ".chibipop-test-install.json"


def assert_safe_test_install_dir(path: Path, repo_root: Path) -> Path:
    root = repo_root.resolve()
    resolved = normalize_path(path, root)
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise ValueError("--test-install-dir must be inside --repo-root") from exc
    if resolved == root:
        raise ValueError("--test-install-dir must not be the repository root")
    blocked = [
        root / ".git",
        root / "target",
        root / "data",
        root / "docs",
        root / "src",
        root / "crates",
        root / "scripts",
        root / "tests",
        root / "plugins",
        root / ".github",
    ]
    for item in blocked:
        blocked_root = item.resolve()
        try:
            resolved.relative_to(blocked_root)
        except ValueError:
            continue
        raise ValueError(f"--test-install-dir cannot be inside {blocked_root}")
    return resolved


def test_install_lock_path(args: argparse.Namespace) -> Path:
    install_dir = assert_safe_test_install_dir(args.test_install_dir, args.repo_root)
    return install_dir.with_name(install_dir.name + ".lock")


def acquire_test_install_lock(args: argparse.Namespace) -> Path:
    lock = test_install_lock_path(args)
    lock.parent.mkdir(parents=True, exist_ok=True)
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError as exc:
        raise ValueError(f"test install is already in use: {lock}") from exc
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump({"pid": os.getpid(), "created_at": dt.datetime.now(dt.timezone.utc).isoformat()}, handle)
    return lock


def release_test_install_lock(args: argparse.Namespace) -> None:
    lock = getattr(args, "test_install_lock_path", None)
    if not lock:
        return
    try:
        Path(lock).unlink()
    except FileNotFoundError:
        pass


def seed_test_install(args: argparse.Namespace, logs_dir: Path) -> tuple[Target, Result]:
    start = time.perf_counter()
    lock = acquire_test_install_lock(args)
    args.test_install_lock_path = lock
    code, out, build_elapsed, build_log = run_cmd(
        [args.cargo, "build", "--release", "--workspace", "--exclude", "chibipop-linux"],
        args.repo_root,
        logs_dir,
        "preflight-test-install-release-build",
    )
    if code != 0:
        target = Target("test-install", args.repo_root / args.test_install_dir / "chibipop.exe", True)
        result = Result(
            "preflight.test-install",
            "preflight",
            "Create disposable test install",
            "auto",
            STATUS_FAIL,
            f"release build exited {code}",
            time.perf_counter() - start,
            {"build_log": rel(build_log, args.repo_root), "build_seconds": build_elapsed},
        )
        return target, result

    install_dir = assert_safe_test_install_dir(args.test_install_dir, args.repo_root)
    marker = install_dir / TEST_INSTALL_MARKER
    if install_dir.exists():
        if not install_dir.is_dir():
            raise ValueError(f"refusing to replace non-directory test install path: {install_dir}")
        if not marker_identifies_test_install(marker, install_dir):
            raise ValueError(f"refusing to replace unmarked test install directory: {install_dir}")
        shutil.rmtree(install_dir)

    exe_name = "chibipop.exe" if os.name == "nt" else "chibipop"
    copies = [
        (args.repo_root / "target" / "release" / exe_name, install_dir / exe_name),
        (args.repo_root / "data" / "deconjugator.json", install_dir / "data" / "deconjugator.json"),
        (args.repo_root / "README.md", install_dir / "README.md"),
        (args.repo_root / "LICENSE", install_dir / "LICENSE"),
        (args.repo_root / "plugins" / "meikiocr" / "plugin.toml", install_dir / "plugins" / "meikiocr" / "plugin.toml"),
        (args.repo_root / "plugins" / "meikiocr" / "adapter.py", install_dir / "plugins" / "meikiocr" / "adapter.py"),
        (args.repo_root / "plugins" / "meikiocr" / "config.toml", install_dir / "plugins" / "meikiocr" / "config.toml"),
    ]
    copied = []
    for src, dst in copies:
        if not src.exists():
            raise FileNotFoundError(f"required package file missing: {src}")
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        copied.append(dst.relative_to(install_dir).as_posix())
    marker.write_text(
        json.dumps(
            {
                "schema": "chibipop-test-install/v1",
                "root": str(install_dir),
                "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "source": rel(args.repo_root / "target" / "release" / exe_name, args.repo_root),
                "files": copied,
            },
            indent=2,
            sort_keys=True,
        ),
        encoding="utf-8",
    )
    target = Target("test-install", install_dir / exe_name, True)
    result = Result(
        "preflight.test-install",
        "preflight",
        "Create disposable test install",
        "auto",
        STATUS_PASS,
        "seeded disposable install from release build",
        time.perf_counter() - start,
        {
            "root": rel(install_dir, args.repo_root),
            "exe": rel(target.exe, args.repo_root),
            "build_log": rel(build_log, args.repo_root),
            "files": copied,
        },
    )
    return target, result


def cleanup_test_install(args: argparse.Namespace) -> Result | None:
    if not args.test_install or args.keep_test_install:
        return None
    start = time.perf_counter()
    install_dir = assert_safe_test_install_dir(args.test_install_dir, args.repo_root)
    marker = install_dir / TEST_INSTALL_MARKER
    if not install_dir.exists():
        return Result("postflight.test-install", "postflight", "Remove disposable test install", "auto", STATUS_PASS, "already absent", 0.0)
    if not marker_identifies_test_install(marker, install_dir):
        return Result("postflight.test-install", "postflight", "Remove disposable test install", "auto", STATUS_FAIL, f"marker missing or invalid in {install_dir}")
    try:
        shutil.rmtree(install_dir)
    except OSError as exc:
        return Result("postflight.test-install", "postflight", "Remove disposable test install", "auto", STATUS_FAIL, f"remove failed: {exc}", time.perf_counter() - start, {"root": rel(install_dir, args.repo_root)})
    return Result("postflight.test-install", "postflight", "Remove disposable test install", "auto", STATUS_PASS, "removed disposable install", time.perf_counter() - start, {"root": rel(install_dir, args.repo_root)})


def parse_points(values: Iterable[str]) -> dict[str, tuple[int, int]]:
    points: dict[str, tuple[int, int]] = {}
    for value in values:
        if "=" not in value:
            raise ValueError(f"--probe-point must be NAME=X,Y, got {value!r}")
        name, raw = value.split("=", 1)
        x_text, y_text = raw.split(",", 1)
        points[name] = (int(x_text), int(y_text))
    return points


def normalize_path(path: Path, base: Path) -> Path:
    candidate = path if path.is_absolute() else base / path
    return candidate.resolve()


def marker_identifies_test_install(marker: Path, install_dir: Path) -> bool:
    if not marker.exists():
        return False
    try:
        data = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    if not isinstance(data, dict):
        return False
    if data.get("schema") != "chibipop-test-install/v1":
        return False
    root_text = data.get("root")
    if not isinstance(root_text, str):
        return False
    try:
        return Path(root_text).resolve() == install_dir.resolve()
    except OSError:
        return False


def add_point_alias(points: dict[str, tuple[int, int]], name: str, value: str) -> None:
    if not value or name in points:
        return
    parsed = parse_points([f"{name}={value}"])
    points.update(parsed)


def target_processes_under_repo_target(repo_root: Path) -> list[dict[str, object]]:
    if os.name != "nt":
        return []
    ps = (
        "Get-CimInstance Win32_Process | "
        "Where-Object { $_.Name -eq 'chibipop.exe' } | "
        "Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress"
    )
    proc = subprocess.run(["powershell", "-NoProfile", "-Command", ps], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if proc.returncode != 0 or not proc.stdout.strip():
        return []
    data = json.loads(proc.stdout)
    rows = data if isinstance(data, list) else [data]
    needle = str((repo_root / "target").resolve()).casefold()
    return [row for row in rows if str(row.get("ExecutablePath", "")).casefold().startswith(needle)]


def stop_target_strays(repo_root: Path) -> Result:
    start = time.perf_counter()
    rows = target_processes_under_repo_target(repo_root)
    stopped: list[int] = []
    for row in rows:
        pid = int(row["ProcessId"])
        subprocess.run(["powershell", "-NoProfile", "-Command", f"Stop-Process -Id {pid} -Force"], text=True)
        stopped.append(pid)
    return Result("preflight.stop-target-strays", "preflight", "Stop repo target chibipop strays", "auto", STATUS_PASS, f"stopped {len(stopped)} target process(es)", time.perf_counter() - start, {"pids": stopped})


def hash_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def snapshot_target(target: Target) -> dict[str, str]:
    patterns = [
        "chibipop.toml",
        "popup.css",
        "data/chibipop.sqlite",
        "data/chibipop.sqlite-*",
        "data/chibipop.sqlite.*",
        "library/**/*",
        "screenshots/**/*",
        "plugins/*/config.toml",
    ]
    found: dict[str, str] = {}
    for pattern in patterns:
        for path in target.root.glob(pattern):
            if path.is_file():
                rel = path.relative_to(target.root).as_posix()
                try:
                    found[rel] = hash_file(path)
                except OSError as exc:
                    found[rel] = f"unreadable:{exc}"
    return dict(sorted(found.items()))


def restore_protected_state(target: Target, backup_root: Path) -> tuple[str, dict[str, object]]:
    if not backup_root.exists():
        return "not_applicable", {"reason": "no backup directory"}
    manifest_path = backup_root / "manifest.json"
    if not manifest_path.exists():
        return "failed", {"reason": "backup manifest missing"}
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    errors: list[str] = []
    backed_up = {entry["path"] for entry in manifest.get("files", [])}
    protected = set(snapshot_target(target))
    for rel_path in sorted(protected - backed_up):
        path = target.root / rel_path
        try:
            path.unlink()
        except OSError as exc:
            errors.append(f"remove {rel_path}: {exc}")
    for entry in manifest.get("files", []):
        rel_path = entry["path"]
        src = backup_root / rel_path
        dst = target.root / rel_path
        try:
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
        except OSError as exc:
            errors.append(f"restore {rel_path}: {exc}")
    if errors:
        return "failed", {"errors": errors}
    return "restored", {"files": len(backed_up)}


def backup_protected_state(target: Target, artifacts_dir: Path) -> tuple[Path, dict[str, str]]:
    snapshot = snapshot_target(target)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup_root = artifacts_dir / "protected-state" / f"{safe_name(target.name)}-{stamp}"
    suffix = 1
    while backup_root.exists():
        backup_root = artifacts_dir / "protected-state" / f"{safe_name(target.name)}-{stamp}-{suffix}"
        suffix += 1
    files = []
    for rel_path, digest in snapshot.items():
        src = target.root / rel_path
        dst = backup_root / rel_path
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        files.append({"path": rel_path, "sha256": digest})
    backup_root.mkdir(parents=True, exist_ok=True)
    (backup_root / "manifest.json").write_text(
        json.dumps({"target": target.name, "files": files}, indent=2, sort_keys=True),
        encoding="utf-8",
    )
    return backup_root, snapshot


def diff_snapshot(before: dict[str, str], after: dict[str, str]) -> dict[str, list[str]]:
    keys = set(before) | set(after)
    return {
        "added": sorted(k for k in keys if k not in before),
        "removed": sorted(k for k in keys if k not in after),
        "changed": sorted(k for k in keys if k in before and k in after and before[k] != after[k]),
    }


def rel(path: Path, root: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return str(path)


def creationflags_for_detached_gui() -> int:
    if os.name != "nt":
        return 0
    return 0x00000008  # DETACHED_PROCESS


def launch_logged_process(cmd: list[str | os.PathLike[str]], cwd: Path, logs_dir: Path, name: str) -> tuple[subprocess.Popen[str], Path, object]:
    logs_dir.mkdir(parents=True, exist_ok=True)
    log_path = logs_dir / f"{safe_name(name)}.log"
    handle = log_path.open("w", encoding="utf-8", errors="replace")
    proc = subprocess.Popen(
        [str(part) for part in cmd],
        cwd=str(cwd),
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=handle,
        stderr=subprocess.STDOUT,
        creationflags=creationflags_for_detached_gui(),
    )
    return proc, log_path, handle


class Win32Desktop:
    WM_CLOSE = 0x0010
    WM_COMMAND = 0x0111
    CB_GETCOUNT = 0x0146
    CB_GETLBTEXT = 0x0148
    CB_GETLBTEXTLEN = 0x0149

    def __init__(self) -> None:
        import ctypes.wintypes as wt

        self.wt = wt
        self.user32 = ctypes.windll.user32
        self.user32.EnumWindows.argtypes = [ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM), wt.LPARAM]
        self.user32.EnumWindows.restype = wt.BOOL
        self.user32.EnumChildWindows.argtypes = [wt.HWND, ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM), wt.LPARAM]
        self.user32.EnumChildWindows.restype = wt.BOOL
        self.user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
        self.user32.GetWindowThreadProcessId.restype = wt.DWORD
        self.user32.GetClassNameW.argtypes = [wt.HWND, wt.LPWSTR, ctypes.c_int]
        self.user32.GetClassNameW.restype = ctypes.c_int
        self.user32.GetWindowTextLengthW.argtypes = [wt.HWND]
        self.user32.GetWindowTextLengthW.restype = ctypes.c_int
        self.user32.GetWindowTextW.argtypes = [wt.HWND, wt.LPWSTR, ctypes.c_int]
        self.user32.GetWindowTextW.restype = ctypes.c_int
        self.user32.GetDlgCtrlID.argtypes = [wt.HWND]
        self.user32.GetDlgCtrlID.restype = ctypes.c_int
        self.user32.IsWindowVisible.argtypes = [wt.HWND]
        self.user32.IsWindowVisible.restype = wt.BOOL
        self.user32.IsWindowEnabled.argtypes = [wt.HWND]
        self.user32.IsWindowEnabled.restype = wt.BOOL
        self.user32.SendMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
        self.user32.SendMessageW.restype = wt.LPARAM
        self.user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
        self.user32.PostMessageW.restype = wt.BOOL

    def class_name(self, hwnd: int) -> str:
        buf = ctypes.create_unicode_buffer(256)
        self.user32.GetClassNameW(self.wt.HWND(hwnd), buf, len(buf))
        return buf.value

    def text(self, hwnd: int) -> str:
        length = self.user32.GetWindowTextLengthW(self.wt.HWND(hwnd))
        buf = ctypes.create_unicode_buffer(length + 1)
        self.user32.GetWindowTextW(self.wt.HWND(hwnd), buf, len(buf))
        return buf.value

    def pid_of(self, hwnd: int) -> int:
        pid = self.wt.DWORD()
        self.user32.GetWindowThreadProcessId(self.wt.HWND(hwnd), ctypes.byref(pid))
        return int(pid.value)

    def windows_for_pid(self, pid: int, include_children: bool = False) -> list[dict[str, object]]:
        rows: list[dict[str, object]] = []

        def add(hwnd: int) -> None:
            if self.pid_of(hwnd) != pid:
                return
            rows.append(
                {
                    "hwnd": hwnd,
                    "class": self.class_name(hwnd),
                    "text": self.text(hwnd),
                    "id": self.user32.GetDlgCtrlID(self.wt.HWND(hwnd)),
                    "visible": bool(self.user32.IsWindowVisible(self.wt.HWND(hwnd))),
                    "enabled": bool(self.user32.IsWindowEnabled(self.wt.HWND(hwnd))),
                }
            )

        callback = ctypes.WINFUNCTYPE(self.wt.BOOL, self.wt.HWND, self.wt.LPARAM)

        @callback
        def enum_top(hwnd, _param):
            add(int(hwnd))
            if include_children:
                @callback
                def enum_child(child, _child_param):
                    add(int(child))
                    return True

                self.user32.EnumChildWindows(hwnd, enum_child, 0)
            return True

        self.user32.EnumWindows(enum_top, 0)
        return rows

    def wait_for_class(self, pid: int, class_name: str, timeout: float = 10.0) -> dict[str, object] | None:
        deadline = time.perf_counter() + timeout
        while time.perf_counter() < deadline:
            for row in self.windows_for_pid(pid):
                if row["class"] == class_name:
                    return row
            time.sleep(0.1)
        return None

    def post_close(self, hwnd: int) -> None:
        self.user32.PostMessageW(self.wt.HWND(hwnd), self.WM_CLOSE, 0, 0)

    def post_command(self, hwnd: int, command_id: int) -> None:
        self.user32.PostMessageW(self.wt.HWND(hwnd), self.WM_COMMAND, command_id, 0)

    def combo_items(self, hwnd: int) -> list[str]:
        count = int(self.user32.SendMessageW(self.wt.HWND(hwnd), self.CB_GETCOUNT, 0, 0))
        if count < 0:
            return []
        items = []
        for index in range(count):
            length = int(self.user32.SendMessageW(self.wt.HWND(hwnd), self.CB_GETLBTEXTLEN, index, 0))
            if length < 0:
                continue
            buf = ctypes.create_unicode_buffer(length + 1)
            self.user32.SendMessageW(
                self.wt.HWND(hwnd),
                self.CB_GETLBTEXT,
                index,
                self.wt.LPARAM(ctypes.addressof(buf)),
            )
            items.append(buf.value)
        return items


def parse_test_counts(output: str) -> dict[str, int]:
    counts = {"passed": 0, "failed": 0, "ignored": 0}
    for match in re.finditer(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored", output):
        counts["passed"] += int(match.group(1))
        counts["failed"] += int(match.group(2))
        counts["ignored"] += int(match.group(3))
    return counts


def grep_count(output: str, pattern: str, exclude: str | None = None) -> int:
    rx = re.compile(pattern)
    ex = re.compile(exclude) if exclude else None
    total = 0
    for line in output.splitlines():
        if rx.search(line) and not (ex and ex.search(line)):
            total += 1
    return total


def auto_cargo_tests(check: Check, args: argparse.Namespace, logs_dir: Path) -> Result:
    start = time.perf_counter()
    evidence = []
    last_counts = {"passed": 0, "failed": 0, "ignored": 0}
    status = STATUS_PASS
    detail = ""
    for index in range(1, args.repeat_tests + 1):
        code, out, elapsed, log = run_cmd([args.cargo, "test", "--workspace", "--exclude", "chibipop-linux"], args.repo_root, logs_dir, f"tier0-test-{index}")
        counts = parse_test_counts(out)
        last_counts = counts
        evidence.append({"run": index, "exit_code": code, "seconds": elapsed, "counts": counts, "log": rel(log, args.repo_root)})
        if code != 0:
            golden_only = args.allow_local_golden_failure and counts["failed"] == 1 and "geometry_golden_full_chrome" in out
            if not golden_only:
                status = STATUS_FAIL
                detail = f"cargo test run {index} exited {code}"
                break
    if status == STATUS_PASS:
        if last_counts["passed"] < args.min_test_total:
            status = STATUS_FAIL
            detail = f"last run passed {last_counts['passed']}; expected at least {args.min_test_total}"
        else:
            detail = f"last run passed {last_counts['passed']} test(s), failed {last_counts['failed']}, ignored {last_counts['ignored']}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, time.perf_counter() - start, {"runs": evidence})


def auto_clippy_accepted(check: Check, args: argparse.Namespace, logs_dir: Path) -> Result:
    cmd = [
        args.cargo,
        "clippy",
        "--workspace",
        "--color",
        "never",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, "tier0-clippy-accepted")
    count = grep_count(out, r"^error", r"could not compile")
    status = STATUS_PASS if count == args.expected_clippy_warnings else STATUS_FAIL
    detail = f"accepted clippy error count {count}; expected {args.expected_clippy_warnings}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, {"exit_code": code, "count": count, "log": rel(log, args.repo_root)})


def auto_clippy_suppressed(check: Check, args: argparse.Namespace, logs_dir: Path) -> Result:
    cmd = [
        args.cargo,
        "clippy",
        "--workspace",
        "--color",
        "never",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
        "-A",
        "clippy::while_let_loop",
        "-A",
        "clippy::doc_lazy_continuation",
        "-A",
        "clippy::useless_conversion",
        "-A",
        "clippy::too_many_arguments",
        "-A",
        "clippy::needless_lifetimes",
        "-A",
        "clippy::type_complexity",
    ]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, "tier0-clippy-suppressed")
    count = grep_count(out, r"^error", r"could not compile")
    status = STATUS_PASS if count == args.expected_other_clippy else STATUS_FAIL
    detail = f"other clippy finding count {count}; expected {args.expected_other_clippy}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, {"exit_code": code, "count": count, "log": rel(log, args.repo_root)})


def auto_release_build(check: Check, args: argparse.Namespace, logs_dir: Path) -> Result:
    code, out, elapsed, log = run_cmd([args.cargo, "build", "--release", "--workspace", "--exclude", "chibipop-linux"], args.repo_root, logs_dir, "tier0-release-build")
    status = STATUS_PASS if code == 0 else STATUS_FAIL
    detail = "release build finished" if code == 0 else f"release build exited {code}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, {"exit_code": code, "log": rel(log, args.repo_root)})


def point_arg(points: dict[str, tuple[int, int]], name: str) -> str | None:
    point = points.get(name)
    if not point:
        return None
    return f"{point[0]},{point[1]}"


def first_target(targets: list[Target]) -> Target | None:
    return targets[0] if targets else None


def auto_probe_pipeline(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    target = first_target(targets)
    point = point_arg(points, "pipeline")
    if not target or not point:
        return unavailable(check, "needs --target and --probe-point pipeline=X,Y")
    cmd = [target.exe, "probe", "--at", point, "--tiles", "1"]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, "tier1-1.1-probe-pipeline")
    required = ["orient:", "line:", "at:", "anchor:", "match:"]
    missing = [text for text in required if text not in out]
    ranked = bool(re.search(r"(?m)^\s*\d+[.)]|\bhits?\b", out))
    if not ranked:
        missing.append("ranked hits")
    status = STATUS_PASS if code == 0 and not missing else STATUS_FAIL
    detail = "pipeline markers present" if status == STATUS_PASS else f"missing {', '.join(missing)}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, {"command": command_text(cmd), "log": rel(log, args.repo_root)})


def auto_probe_show_region(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    target = first_target(targets)
    point = point_arg(points, "draw")
    if not target or not point:
        return unavailable(check, "needs --target and --probe-point draw=X,Y")
    cmd = [target.exe, "probe", "--at", point, "--tiles", "1", "--show-region", str(args.show_region_seconds)]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, "tier1-1.4-probe-show-region")
    status = STATUS_PASS if code == 0 and "match:" in out else STATUS_FAIL
    detail = "probe displayed region; visual inspection still required" if status == STATUS_PASS else f"probe exited {code}"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, {"command": command_text(cmd), "log": rel(log, args.repo_root)})


def auto_probe_stability(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    target = first_target(targets)
    base = points.get("highlight") or points.get("pipeline")
    if not target or not base:
        return unavailable(check, "needs --target and --probe-point highlight=X,Y or pipeline=X,Y")
    offsets = [(0, 0), (1, 0), (0, 1), (1, 1)]
    signatures = []
    runs = []
    for index, (dx, dy) in enumerate(offsets, 1):
        point = f"{base[0] + dx},{base[1] + dy}"
        cmd = [target.exe, "probe", "--at", point, "--tiles", "1"]
        code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, f"tier1-1.5-probe-stability-{index}")
        hits = "\n".join(line for line in out.splitlines() if re.search(r"^\s*\d+[.)]|\bmatch:", line))
        signatures.append(hits)
        runs.append({"point": point, "exit_code": code, "seconds": elapsed, "log": rel(log, args.repo_root)})
        if code != 0:
            return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, f"probe {index} exited {code}", 0.0, {"runs": runs})
    status = STATUS_PASS if len(set(signatures)) == 1 else STATUS_FAIL
    detail = "hit signatures identical across four nudges" if status == STATUS_PASS else "hit signatures differed across nudges"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, 0.0, {"runs": runs})


def auto_probe_vertical(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    target = first_target(targets)
    point = point_arg(points, "vertical")
    if not target or not point:
        return unavailable(check, "needs --target and --probe-point vertical=X,Y")
    runs = []
    for region in ["500,100", "100,500"]:
        cmd = [target.exe, "probe", "--at", point, "--region", region]
        code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, f"tier1-1.6-probe-vertical-{region}")
        runs.append({"region": region, "exit_code": code, "seconds": elapsed, "log": rel(log, args.repo_root), "has_match": "match:" in out})
    detail = "recorded both vertical probe shapes; classify output against the documented known ceiling"
    return Result(check.ident, check.tier, check.title, check.mode, STATUS_XFAIL, detail, 0.0, {"runs": runs})


def auto_probe_outlined(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    target = first_target(targets)
    outlined = point_arg(points, "outlined")
    solid = point_arg(points, "solid")
    if not target or not outlined or not solid:
        return unavailable(check, "needs --target plus --probe-point outlined=X,Y and solid=X,Y")
    runs = []
    for name, point in [("outlined", outlined), ("solid", solid)]:
        cmd = [target.exe, "probe", "--at", point, "--region", "820,60", "--upscale", "1"]
        code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, f"tier1-1.7a-{name}")
        line0 = next((line for line in out.splitlines() if "ocr line 0:" in line), "")
        runs.append({"name": name, "exit_code": code, "seconds": elapsed, "ocr_line_0": line0, "log": rel(log, args.repo_root)})
    return Result(check.ident, check.tier, check.title, check.mode, STATUS_XFAIL, "recorded outlined and solid OCR lines for same-run scoring", 0.0, {"runs": runs})


def auto_resources(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target]) -> Result:
    target = first_target(targets) or default_target(args.repo_root)
    if not target.exe.exists():
        return unavailable(check, f"missing target exe {target.exe}")
    size = target.exe.stat().st_size
    status = STATUS_PASS if size < 100 * 1024 * 1024 else STATUS_FAIL
    detail = f"exe size {size} bytes"
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, 0.0, {"exe": rel(target.exe, args.repo_root), "bytes": size})


def find_controls_by_id(node: object, control_id: int, out: list[dict[str, object]]) -> None:
    if isinstance(node, dict):
        if node.get("id") == control_id:
            out.append(node)
        for value in node.values():
            find_controls_by_id(value, control_id, out)
    elif isinstance(node, list):
        for value in node:
            find_controls_by_id(value, control_id, out)


def auto_settings_audit(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target]) -> Result:
    target = first_target(targets)
    if not target:
        return unavailable(check, "needs --target")
    seeded = ensure_fixture_database(target, args, logs_dir)
    if seeded is not None and seeded.status != STATUS_PASS:
        return Result(check.ident, check.tier, check.title, check.mode, seeded.status, seeded.detail, seeded.seconds, seeded.evidence)
    cmd = [target.exe, "settings", "--audit"]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, f"tier1-{check.ident}-settings-audit")
    if code != 0:
        return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, f"settings --audit exited {code}", elapsed, {"log": rel(log, args.repo_root)})
    try:
        data = json.loads(out)
    except json.JSONDecodeError as exc:
        return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, f"audit output is not JSON: {exc}", elapsed, {"log": rel(log, args.repo_root)})
    controls: list[dict[str, object]] = []
    find_controls_by_id(data, 100, controls)
    ys = []
    for control in controls:
        rect = control.get("rect")
        if isinstance(rect, dict) and "y" in rect:
            ys.append(rect["y"])
    same = bool(ys) and len(set(ys)) == 1
    status = STATUS_PASS if same else STATUS_FAIL
    detail = f"Apply control y values: {ys}" if ys else "Apply control id 100 not found"
    evidence = {"log": rel(log, args.repo_root), "apply_y": ys}
    if seeded is not None:
        evidence["fixture_db"] = seeded.status
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, evidence)


def audit_texts(data: object) -> list[str]:
    texts: list[str] = []
    if isinstance(data, dict):
        text = data.get("text")
        if isinstance(text, str) and text:
            texts.append(text)
        for value in data.values():
            texts.extend(audit_texts(value))
    elif isinstance(data, list):
        for value in data:
            texts.extend(audit_texts(value))
    return texts


def run_settings_audit(target: Target, args: argparse.Namespace, logs_dir: Path, name: str) -> tuple[int, object | None, float, Path, str]:
    cmd = [target.exe, "settings", "--audit"]
    code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, name)
    data: object | None = None
    if code == 0:
        try:
            data = json.loads(out)
        except json.JSONDecodeError:
            data = None
    return code, data, elapsed, log, out


def ensure_fixture_database(target: Target, args: argparse.Namespace, logs_dir: Path) -> Result | None:
    db = target.root / "data" / "chibipop.sqlite"
    if db.exists():
        return None
    if not target.disposable:
        return Result("preflight.fixture-db", "preflight", "Seed fixture dictionary", "auto", STATUS_SKIP, "only disposable targets can be fixture-seeded")
    archive = args.repo_root / "tests" / "fixtures" / "yomitan" / "terms.zip"
    if not archive.exists():
        return Result("preflight.fixture-db", "preflight", "Seed fixture dictionary", "auto", STATUS_SKIP, "missing tests/fixtures/yomitan/terms.zip")
    library = target.root / "library"
    library.mkdir(parents=True, exist_ok=True)
    db.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(archive, library / archive.name)
    code, out, elapsed, log = run_cmd(
        [target.exe, "build-dict", "--library", library, "--out", db],
        target.root,
        logs_dir,
        "preflight-fixture-db",
    )
    status = STATUS_PASS if code == 0 and db.exists() else STATUS_FAIL
    detail = "seeded fixture dictionary" if status == STATUS_PASS else f"build-dict exited {code}"
    return Result("preflight.fixture-db", "preflight", "Seed fixture dictionary", "auto", status, detail, elapsed, {"log": rel(log, args.repo_root)})


def auto_fresh_meikiocr_combo(check: Check, args: argparse.Namespace, logs_dir: Path, target: Target) -> Result:
    seeded = ensure_fixture_database(target, args, logs_dir)
    if seeded is not None and seeded.status != STATUS_PASS:
        return Result(check.ident, check.tier, check.title, check.mode, seeded.status, seeded.detail, seeded.seconds, seeded.evidence)
    if os.name != "nt":
        return unavailable(check, "Win32 combo inspection requires Windows")
    proc, log, handle = launch_logged_process([target.exe, "settings"], target.root, logs_dir, f"tier1-{check.ident}-fresh-meikiocr-combo")
    start = time.perf_counter()
    desktop = Win32Desktop()
    try:
        window = desktop.wait_for_class(proc.pid, "ChibipopSettingsClass", timeout=10.0)
        if window is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=3)
            return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, "settings window did not appear", time.perf_counter() - start, {"pid": proc.pid, "log": rel(log, args.repo_root)})
        engine = None
        deadline = time.perf_counter() + 5.0
        while time.perf_counter() < deadline and engine is None:
            rows = desktop.windows_for_pid(proc.pid, include_children=True)
            engine = next((row for row in rows if row["id"] == 146), None)
            if engine is None:
                time.sleep(0.1)
        if engine is None:
            status = STATUS_FAIL
            detail = "engine combo id 146 not found"
            items: list[str] = []
        else:
            items = desktop.combo_items(int(engine["hwnd"]))
            missing = [item for item in ["Built-in (Windows OCR)", "meikiocr"] if item not in items]
            status = STATUS_PASS if not missing else STATUS_FAIL
            detail = "engine combo lists Built-in and meikiocr" if status == STATUS_PASS else f"engine combo missing {', '.join(missing)}"
        desktop.post_close(int(window["hwnd"]))
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)
        return Result(
            check.ident,
            check.tier,
            check.title,
            check.mode,
            status,
            detail,
            time.perf_counter() - start,
            {"pid": proc.pid, "items": items, "fixture_db": seeded.status if seeded else "already_present", "log": rel(log, args.repo_root)},
        )
    finally:
        if proc.poll() is None:
            proc.kill()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass
        handle.close()


def auto_fresh_meikiocr_audit(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target]) -> Result:
    target = first_target(targets)
    if not target:
        return unavailable(check, "needs --target")
    if not target.disposable:
        return unavailable(check, "fresh-install check requires --test-install")
    if check.ident == "1.28.3":
        return auto_fresh_meikiocr_combo(check, args, logs_dir, target)
    seeded = ensure_fixture_database(target, args, logs_dir)
    if seeded is not None and seeded.status != STATUS_PASS:
        return Result(check.ident, check.tier, check.title, check.mode, seeded.status, seeded.detail, seeded.seconds, seeded.evidence)
    code, data, elapsed, log, out = run_settings_audit(target, args, logs_dir, f"tier1-{check.ident}-fresh-meikiocr-audit")
    if code != 0:
        return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, f"settings --audit exited {code}", elapsed, {"log": rel(log, args.repo_root)})
    if data is None:
        return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, "audit output is not JSON", elapsed, {"log": rel(log, args.repo_root)})
    texts = audit_texts(data)
    dumps = data.get("dumps", []) if isinstance(data, dict) else []
    requirements = {
        "1.28.2": [],
        "1.28.4": ["meikiocr 0.1.0"],
    }
    missing = [item for item in requirements.get(check.ident, []) if not any(item in text for text in texts)]
    if check.ident == "1.28.2" and len(dumps) < 5:
        missing.append("five tab dumps")
    if "No plugins found" in "\n".join(texts):
        missing.append("plugin discovery without No plugins found")
    status = STATUS_PASS if not missing else STATUS_FAIL
    detail = "fresh install audit found meikiocr controls" if status == STATUS_PASS else f"missing {', '.join(missing)}"
    evidence = {"log": rel(log, args.repo_root), "text_count": len(texts), "dump_count": len(dumps)}
    if seeded is not None:
        evidence["fixture_db"] = seeded.status
    return Result(check.ident, check.tier, check.title, check.mode, status, detail, elapsed, evidence)


def auto_settings_desktop_smoke(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target]) -> Result:
    target = first_target(targets)
    if not target:
        return unavailable(check, "needs --target")
    if os.name != "nt":
        return unavailable(check, "Win32 desktop automation requires Windows")
    seeded = ensure_fixture_database(target, args, logs_dir)
    if seeded is not None and seeded.status == STATUS_FAIL:
        return Result(check.ident, check.tier, check.title, check.mode, seeded.status, seeded.detail, seeded.seconds, seeded.evidence)
    command = [target.exe] if check.ident == "2.11d" else [target.exe, "settings"]
    proc, log, handle = launch_logged_process(command, target.root, logs_dir, f"tier{check.tier}-{check.ident}-settings-desktop")
    start = time.perf_counter()
    desktop = Win32Desktop()
    try:
        window = desktop.wait_for_class(proc.pid, "ChibipopSettingsClass", timeout=10.0)
        if window is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
            return Result(check.ident, check.tier, check.title, check.mode, STATUS_FAIL, "settings window did not appear", time.perf_counter() - start, {"pid": proc.pid, "log": rel(log, args.repo_root)})
        rows = desktop.windows_for_pid(proc.pid, include_children=True)
        classes = sorted({str(row["class"]) for row in rows if row["class"]})
        console_rows = [row for row in rows if row["class"] == "ConsoleWindowClass"]
        if check.ident == "2.11d":
            visible_consoles = [row for row in console_rows if row["visible"]]
            status = STATUS_PASS if not visible_consoles and bool(window["visible"]) else STATUS_FAIL
            detail = "settings visible; no visible owned console" if status == STATUS_PASS else "visible owned console or hidden settings window"
        elif check.ident == "2.11c":
            desktop.post_command(int(window["hwnd"]), 116)
            try:
                proc.wait(timeout=5)
                status = STATUS_PASS
                detail = "Quit command closed standalone settings process"
            except subprocess.TimeoutExpired:
                status = STATUS_FAIL
                detail = "process stayed alive after Quit command"
                proc.kill()
        else:
            desktop.post_close(int(window["hwnd"]))
            try:
                proc.wait(timeout=5)
                status = STATUS_MANUAL
                detail = "WM_CLOSE closed standalone settings; normal run route still needs manual confirmation"
            except subprocess.TimeoutExpired:
                status = STATUS_FAIL
                detail = "process stayed alive after WM_CLOSE"
                proc.kill()
        return Result(
            check.ident,
            check.tier,
            check.title,
            check.mode,
            status,
            detail,
            time.perf_counter() - start,
            {"pid": proc.pid, "settings_hwnd": window["hwnd"], "classes": classes, "log": rel(log, args.repo_root), "fixture_db": seeded.status if seeded else "already_present"},
        )
    finally:
        if proc.poll() is None and check.ident == "2.11d":
            if window := desktop.wait_for_class(proc.pid, "ChibipopSettingsClass", timeout=0.1):
                desktop.post_close(int(window["hwnd"]))
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
        handle.close()


def auto_plugin_cli(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target]) -> Result:
    target = first_target(targets)
    if not target:
        return unavailable(check, "needs --target")
    if not args.allow_plugin_fixtures:
        return unavailable(check, "needs --allow-plugin-fixtures")
    image = normalize_path(args.plugin_image, args.repo_root) if args.plugin_image else args.repo_root / "docs" / "fixtures" / "plugin-sample.png"
    if not image.exists():
        return unavailable(check, "missing docs/fixtures/plugin-sample.png")
    plugin_root = target.root / "plugins"
    echo_dir = plugin_root / "echo"
    broken_dir = plugin_root / "broken"
    if echo_dir.exists() or broken_dir.exists():
        return unavailable(check, "plugins/echo or plugins/broken already exists beside target exe")
    plugin_root.mkdir(exist_ok=True)
    try:
        echo_dir.mkdir()
        broken_dir.mkdir()
        (echo_dir / "plugin.toml").write_text(
            "\n".join(
                [
                    'name = "echo"',
                    'version = "0.1.0"',
                    "protocol = 1",
                    f'command = "{target.exe.as_posix()}"',
                    'args = ["plugin-echo", "ok"]',
                    'roles = ["text-provider"]',
                    "",
                    "[text_provider]",
                    "provides_geometry = true",
                    'languages = ["ja"]',
                    "timeout_ms = 2000",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        (broken_dir / "plugin.toml").write_text(
            "\n".join(
                [
                    'name = "broken"',
                    'version = "0.1.0"',
                    "protocol = 1",
                    'roles = ["text-provider"]',
                    "",
                    "[text_provider]",
                    "provides_geometry = true",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        cases = [
            ("list", [target.exe, "plugin", "list"], 1),
            ("test-echo", [target.exe, "plugin", "test", "echo", "--image", image], 0),
            ("test-broken", [target.exe, "plugin", "test", "broken", "--image", image], 1),
            ("test-nosuch", [target.exe, "plugin", "test", "nosuchplugin", "--image", image], 2),
            ("test-missing-image", [target.exe, "plugin", "test", "echo", "--image", args.repo_root / "docs" / "fixtures" / "does-not-exist.png"], 2),
        ]
        evidence = []
        failed = []
        for name, cmd, expected in cases:
            code, out, elapsed, log = run_cmd(cmd, args.repo_root, logs_dir, f"tier1-1.25-plugin-{name}")
            evidence.append({"case": name, "exit_code": code, "expected": expected, "seconds": elapsed, "log": rel(log, args.repo_root)})
            if code != expected:
                failed.append(f"{name}: got {code}, expected {expected}")
        status = STATUS_PASS if not failed else STATUS_FAIL
        detail = "all plugin CLI exit codes matched" if not failed else "; ".join(failed)
        return Result(check.ident, check.tier, check.title, check.mode, status, detail, 0.0, {"cases": evidence})
    finally:
        shutil.rmtree(echo_dir, ignore_errors=True)
        shutil.rmtree(broken_dir, ignore_errors=True)
        try:
            if plugin_root.exists() and not any(plugin_root.iterdir()):
                plugin_root.rmdir()
        except OSError:
            pass


AUTO: dict[str, Callable[..., Result]] = {
    "cargo_tests": auto_cargo_tests,
    "clippy_accepted": auto_clippy_accepted,
    "clippy_suppressed": auto_clippy_suppressed,
    "release_build": auto_release_build,
    "probe_pipeline": auto_probe_pipeline,
    "probe_show_region": auto_probe_show_region,
    "probe_stability": auto_probe_stability,
    "probe_vertical": auto_probe_vertical,
    "probe_outlined": auto_probe_outlined,
    "resources": auto_resources,
    "settings_audit": auto_settings_audit,
    "fresh_meikiocr_audit": auto_fresh_meikiocr_audit,
    "settings_desktop_smoke": auto_settings_desktop_smoke,
    "plugin_cli": auto_plugin_cli,
}


def unavailable(check: Check, reason: str) -> Result:
    return Result(check.ident, check.tier, check.title, check.mode, STATUS_SKIP, reason)


def matches_selector(ident: str, selector: str) -> bool:
    selector = selector.rstrip(".")
    if ident == selector or ident.startswith(selector + "."):
        return True
    if ident.startswith(selector) and len(ident) > len(selector):
        return ident[len(selector)].isalpha()
    return False


def should_run(check: Check, args: argparse.Namespace) -> bool:
    if args.tier != "all" and check.tier != args.tier:
        return False
    if args.only and not any(matches_selector(check.ident, item) for item in args.only):
        return False
    if any(matches_selector(check.ident, item) for item in args.skip):
        return False
    return True


def requires_config_write(check: Check) -> bool:
    prefixes = (
        "1.9",
        "1.10",
        "1.11",
        "1.12",
        "1.13",
        "1.14",
        "1.15",
        "1.16",
        "1.27",
        "1.28.7",
        "1.29.1",
        "1.29.2",
        "1.29.4",
        "1.30.10",
        "2.13",
    )
    return "config" in check.effects or matches_any_prefix(check.ident, prefixes)


def requires_dictionary_mutation(check: Check) -> bool:
    prefixes = ("1.17", "1.18", "1.19", "1.20", "2.12", "2.14")
    return "dictionary" in check.effects or matches_any_prefix(check.ident, prefixes)


def requires_anki_write(check: Check) -> bool:
    exact = {"1.11", "1.11.3", "1.22", "1.30", "1.30.4", "1.30.9"}
    return "anki" in check.effects or check.ident in exact or matches_selector(check.ident, "1.22")


def requires_display_change(check: Check) -> bool:
    return check.ident == "1.26.7"


def matches_any_prefix(ident: str, prefixes: tuple[str, ...]) -> bool:
    return any(matches_selector(ident, prefix) for prefix in prefixes)


def prompt_check(check: Check) -> Result:
    wrapped = textwrap.fill(check.prompt, width=88)
    print()
    print(f"{check.ident} {check.title}")
    print(f"  mode: {check.mode}")
    print(f"  ref:  {check.doc_ref}")
    print(f"  {wrapped}")
    while True:
        answer = input("Result [p]ass/[f]ail/[s]kip/[x]fail/[m]anual, notes after space: ").strip()
        if not answer:
            continue
        key, _, notes = answer.partition(" ")
        key = key.lower()
        mapping = {
            "p": STATUS_PASS,
            "pass": STATUS_PASS,
            "f": STATUS_FAIL,
            "fail": STATUS_FAIL,
            "s": STATUS_SKIP,
            "skip": STATUS_SKIP,
            "x": STATUS_XFAIL,
            "xfail": STATUS_XFAIL,
            "m": STATUS_MANUAL,
            "manual": STATUS_MANUAL,
        }
        if key in mapping:
            return Result(check.ident, check.tier, check.title, check.mode, mapping[key], notes)
        print("Use p, f, s, x, or m.")


def skip_check(check: Check, reason: str) -> Result:
    return Result(check.ident, check.tier, check.title, check.mode, STATUS_SKIP, reason)


def manual_check(check: Check, reason: str) -> Result:
    return Result(check.ident, check.tier, check.title, check.mode, STATUS_MANUAL, reason)


def record_result(results: list[Result], result: Result) -> None:
    results.append(result)
    print(f"{result.status:6} {result.ident:28} {result.title} - {result.detail}")


def run_auto(check: Check, args: argparse.Namespace, logs_dir: Path, targets: list[Target], points: dict[str, tuple[int, int]]) -> Result:
    if not check.auto:
        return unavailable(check, "no automated handler")
    func = AUTO[check.auto]
    if check.auto.startswith("cargo") or check.auto.startswith("clippy") or check.auto == "release_build":
        return func(check, args, logs_dir)  # type: ignore[misc]
    if check.auto.startswith("probe"):
        return func(check, args, logs_dir, targets, points)  # type: ignore[misc]
    return func(check, args, logs_dir, targets)  # type: ignore[misc]


def maybe_open_fixtures(args: argparse.Namespace) -> Result:
    start = time.perf_counter()
    corpus = normalize_path(args.corpus, args.repo_root) if args.corpus else (args.repo_root / "docs" / "fixtures" / "ocr-corpus.html").resolve()
    scroll = normalize_path(args.scroll_fixture, args.repo_root) if args.scroll_fixture else (args.repo_root / "docs" / "fixtures" / "scroll-test.html").resolve()
    urls = [corpus.as_uri(), scroll.as_uri()]
    launched: list[str] = []
    if args.browser_cmd_template:
        import shlex

        splitter_posix = os.name != "nt"
        template = shlex.split(args.browser_cmd_template, posix=splitter_posix)
        for url in urls:
            cmd = [part.replace("{url}", url) for part in template]
            subprocess.Popen(cmd, cwd=str(args.repo_root))
            launched.append(command_text(cmd))
    elif args.browser_command:
        for url in urls:
            cmd = [part.replace("{url}", url) for part in args.browser_command]
            subprocess.Popen(cmd, cwd=str(args.repo_root))
            launched.append(command_text(cmd))
    elif os.name == "nt":
        os.startfile(str(corpus))  # type: ignore[attr-defined]
        os.startfile(str(scroll))  # type: ignore[attr-defined]
        launched.extend([str(corpus), str(scroll)])
    else:
        for url in urls:
            subprocess.Popen(["xdg-open", url], cwd=str(args.repo_root))
            launched.append(url)
    return Result("preflight.open-fixtures", "preflight", "Open regression fixtures", "auto", STATUS_PASS, "opened fixture pages", time.perf_counter() - start, {"launched": launched})


def write_report(args: argparse.Namespace, targets: list[Target], results: list[Result], snapshots: dict[str, dict[str, object]]) -> None:
    report = {
        "schema": "chibipop-regression-report/v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": rel(args.repo_root, args.repo_root),
        "args": {
            "tier": args.tier,
            "only": args.only,
            "skip": args.skip,
            "strict": args.strict,
            "allow_destructive": args.allow_destructive,
            "allow_real_target_destructive": args.allow_real_target_destructive,
            "allow_plugin_fixtures": args.allow_plugin_fixtures,
            "test_install": args.test_install,
            "test_install_dir": rel(normalize_path(args.test_install_dir, args.repo_root), args.repo_root),
            "keep_test_install": args.keep_test_install,
            "repeat_tests": args.repeat_tests,
            "min_test_total": args.min_test_total,
        },
        "targets": [{"name": target.name, "exe": rel(target.exe, args.repo_root), "root": rel(target.root, args.repo_root), "disposable": target.disposable} for target in targets],
        "target_state": snapshots,
        "results": [result.__dict__ for result in results],
        "summary": summarize(results),
    }
    out = args.report
    if not out.is_absolute():
        out = args.repo_root / out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    print(f"wrote report: {out}")


def summarize(results: list[Result]) -> dict[str, int]:
    summary = {STATUS_PASS: 0, STATUS_FAIL: 0, STATUS_SKIP: 0, STATUS_XFAIL: 0, STATUS_MANUAL: 0}
    for result in results:
        summary[result.status] = summary.get(result.status, 0) + 1
    return summary


def main() -> int:
    args = parse_args()
    args.repo_root = args.repo_root.resolve()
    args.artifacts_dir = normalize_path(args.artifacts_dir, args.repo_root)
    if args.report is None:
        args.report = args.artifacts_dir / "regression-report.json"
    if args.logs_dir is None:
        args.logs_dir = args.artifacts_dir / "logs"
    args.dictionary_archive.extend(args.term_archive)
    checks = build_checks()
    selected = [check for check in checks if should_run(check, args)]
    if args.list:
        for check in selected:
            print(f"{check.ident:8} tier {check.tier}  {check.mode:20} {check.title}")
        return 0

    results: list[Result] = []
    logs_dir = args.logs_dir
    if not logs_dir.is_absolute():
        logs_dir = args.repo_root / logs_dir
    targets: list[Target] = []
    snapshots: dict[str, dict[str, object]] = {}
    backups: dict[str, Path] = {}
    run_error = False
    cleanup_failed = False

    try:
        explicit_targets = bool(args.exe or args.secondary_exe or args.target)
        if args.test_install:
            try:
                test_target, result = seed_test_install(args, logs_dir)
            except Exception as exc:
                test_target = Target("test-install", normalize_path(args.test_install_dir, args.repo_root) / ("chibipop.exe" if os.name == "nt" else "chibipop"), True)
                result = Result("preflight.test-install", "preflight", "Create disposable test install", "auto", STATUS_FAIL, str(exc))
            targets.append(test_target)
            record_result(results, result)
            if result.status == STATUS_FAIL:
                run_error = True

        if not run_error:
            if args.exe:
                targets.append(parse_target(f"primary={args.exe}", args.repo_root))
            for index, exe in enumerate(args.secondary_exe, 1):
                targets.append(parse_target(f"secondary{index}={exe}", args.repo_root))
            targets.extend(parse_target(spec, args.repo_root) for spec in args.target)
            if not targets and not explicit_targets:
                targets = [default_target(args.repo_root)]
            if args.test_install and args.allow_destructive and all(target.disposable for target in targets):
                args.allow_config_write = True
                args.allow_dictionary_mutation = True
                args.allow_plugin_fixtures = True
            points = parse_points(args.probe_point)
            add_point_alias(points, "pipeline", args.ja_point)
            add_point_alias(points, "vertical", args.vertical_point)
            add_point_alias(points, "zh_simplified", args.zh_simplified_point)
            add_point_alias(points, "zh_traditional", args.zh_traditional_point)
            add_point_alias(points, "alnum", args.alnum_point)

            has_destructive = any(check.destructive for check in selected)
            if has_destructive and args.allow_destructive:
                args.artifacts_dir.mkdir(parents=True, exist_ok=True)
                for target in targets:
                    backup_root, snapshot = backup_protected_state(target, args.artifacts_dir)
                    backups[target.name] = backup_root
                    snapshots[target.name] = {"before": snapshot, "backup": rel(backup_root, args.repo_root)}

            if args.open_fixtures:
                record_result(results, maybe_open_fixtures(args))

            if args.stop_target_strays:
                record_result(results, stop_target_strays(args.repo_root))

            for target in targets:
                snapshots.setdefault(target.name, {"before": snapshot_target(target)})

        for check in selected:
            if run_error:
                break
            if check.destructive and not args.allow_destructive:
                record_result(results, skip_check(check, "destructive check requires --allow-destructive"))
                continue
            if check.destructive and not args.allow_real_target_destructive and any(not target.disposable for target in targets):
                record_result(results, skip_check(check, "destructive check against non-disposable targets requires --allow-real-target-destructive"))
                continue
            if check.destructive and not args.allow_dictionary_mutation and requires_dictionary_mutation(check):
                record_result(results, skip_check(check, "dictionary check requires --allow-dictionary-mutation"))
                continue
            if check.known_gap and not check.auto and not args.interactive:
                record_result(results, Result(check.ident, check.tier, check.title, check.mode, STATUS_XFAIL, check.prompt))
                continue
            if not args.allow_anki_write and requires_anki_write(check):
                record_result(results, skip_check(check, "Anki-writing check requires --allow-anki-write"))
                continue
            if not args.allow_config_write and requires_config_write(check):
                record_result(results, manual_check(check, "config-writing check requires --allow-config-write for automation; run manually or pass the flag"))
                continue
            if not args.allow_display_change and requires_display_change(check):
                record_result(results, manual_check(check, "display-scaling substep requires --allow-display-change; other substeps remain in the manual instructions"))
                continue
            if check.auto:
                result = run_auto(check, args, logs_dir, targets, points)
                record_result(results, result)
                if result.status not in (STATUS_PASS, STATUS_XFAIL) and args.interactive and check.mode == "auto-or-interactive":
                    record_result(results, prompt_check(check))
                continue
            if args.interactive:
                record_result(results, prompt_check(check))
            else:
                record_result(results, manual_check(check, "interactive check requires --interactive"))
    except Exception as exc:
        run_error = True
        record_result(
            results,
            Result(
                "internal.error",
                "internal",
                "Runner exception",
                "auto",
                STATUS_FAIL,
                f"{type(exc).__name__}: {exc}",
            ),
        )

    if backups and not args.keep_mutated_state:
        for target in targets:
            backup_root = backups.get(target.name)
            if not backup_root:
                continue
            cleanup_status, evidence = restore_protected_state(target, backup_root)
            if cleanup_status == "failed":
                cleanup_failed = True
            record_result(
                results,
                Result(
                    "postflight.restore-protected-state",
                    "postflight",
                    f"Restore protected state for {target.name}",
                    "auto",
                    STATUS_FAIL if cleanup_status == "failed" else STATUS_PASS,
                    cleanup_status,
                    evidence=evidence,
                    cleanup=cleanup_status,
                )
            )

    for target in targets:
        after = snapshot_target(target)
        before = snapshots[target.name].get("before", {})
        snapshots[target.name]["after"] = after
        if isinstance(before, dict):
            diff = diff_snapshot(before, after)
            snapshots[target.name]["diff"] = diff
            if target.disposable:
                continue
            if not args.allow_destructive and any(diff.values()):
                record_result(
                    results,
                    Result(
                        "postflight.protected-state",
                        "postflight",
                        f"Protected state unchanged for {target.name}",
                        "auto",
                        STATUS_FAIL,
                        "protected target files changed without --allow-destructive",
                        evidence=diff,
                    )
                )

    cleanup_result = cleanup_test_install(args)
    if cleanup_result is not None:
        record_result(results, cleanup_result)
    release_test_install_lock(args)

    write_report(args, targets, results, snapshots)

    summary = summarize(results)
    print(json.dumps(summary, sort_keys=True))
    if run_error or cleanup_failed or summary.get(STATUS_FAIL, 0):
        return 1
    if args.strict and (summary.get(STATUS_SKIP, 0) or summary.get(STATUS_MANUAL, 0)):
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
