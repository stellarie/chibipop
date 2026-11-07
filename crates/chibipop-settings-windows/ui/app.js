"use strict";

const { invoke } = window.__TAURI__.core;

const INT_LIMIT = 2147483647;
const NONE = "(none)";
const FIELD_SOURCES = [
  "expression",
  "reading",
  "glossary",
  "frequency",
  "glossary_html",
  "pitch_html",
  "screenshot",
  "sentence",
];
const WIDE_KINDS = new Set(["list", "field-map", "screenshot"]);
const LIBRARY_NOTE =
  "chibipop is using a dictionary built outside the app. Adding or removing here " +
  "rebuilds from this list only. Import the original ZIP files first.";

const THEMES = [
  ["dark", "Dark"],
  ["light", "Light"],
];
const LAYOUTS = [
  ["roomy", "Roomy"],
  ["compact", "Compact"],
];
const SELECTION_BUTTONS = [
  ["primary-additive", "Add to selection"],
  ["primary-replacing", "Replace selection"],
];
const SELECTION_SEPARATORS = [
  ["ellipsis", "Ellipsis (\u2026)"],
  ["space", "Space"],
  ["line-break", "New line"],
  ["list-items", "Separate list items"],
];
const TRIPLE_CLICKS = [
  ["sense", "Meaning"],
  ["sense-with-examples", "Meaning and examples"],
  ["line", "Complete line"],
];
const SENTENCE_MODES = [
  ["sentence", "Detected sentence"],
  ["line", "Line under the pointer"],
  ["all", "All captured lines"],
  ["static", "Fixed screen area"],
];
const SCREENSHOT_MODES = [
  ["region", "Choose a region"],
  ["window", "Choose a window"],
  ["fixed-region", "Reuse one region"],
  ["fixed-window", "Reuse one window"],
];

const VALUES = {
  "dictionary-frequency": (f) => {
    const rows = f.frequency.map((row) => `${row.name} (${row.enabled ? "on" : "off"})`);
    const lists = rows.length > 0 ? rows.join(", ") : "No frequency lists";
    const ranking = f.cfg.dictionaries.ranking_strategy;
    return `${lists}. Ranking: ${ranking}. Read-only: a change needs a reindex.`;
  },
};

function at(path, key) {
  const section = (draft) => path.split(".").reduce((node, name) => node[name], draft.cfg);
  return {
    get: (draft) => section(draft)[key],
    set: (draft, value) => {
      section(draft)[key] = value;
    },
  };
}

const bool = (path, key) => ({ kind: "bool", ...at(path, key) });
const choice = (path, key, options) => ({ kind: "choice", options, ...at(path, key) });
const range = (path, key, limits, unit) => ({
  kind: "number",
  ...limits,
  unit,
  ...at(path, key),
});
const suggest = (key, source) => ({ kind: "suggest", source, ...at("anki", key) });
const sizePart = (label, key) => ({
  label,
  ...at("ocr", key),
  saved: (form) => form.cfg.ocr[key],
});

const EDITORS = {
  "popup-theme": choice("popup", "theme", THEMES),
  "popup-font": { kind: "font", ...at("popup", "font") },
  "popup-custom-style": { kind: "action", disabled: true },
  "popup-max-width": range("popup", "max_width_percent", { min: 10, max: 90, step: 5 }, "%"),
  "popup-max-height": range("popup", "max_height_percent", { min: 10, max: 90, step: 5 }, "%"),
  "popup-summary-length": range(
    "popup",
    "summary_chars",
    { min: 10, max: 200, step: 10 },
    " characters",
  ),
  "popup-highlight": bool("popup", "highlight_match"),
  "popup-scroll": bool("popup", "scroll_popup"),
  "popup-edge-autoscroll": bool("popup", "edge_autoscroll"),
  "popup-side-panel": bool("popup", "side_panel"),
  "popup-hide-on-cursor-exit": bool("popup", "hide_on_cursor_exit"),
  "popup-sub-popups": bool("popup", "sub_popups"),
  "nested-profile": {
    kind: "profile",
    get: (draft) => draft.cfg.nested_profile,
    set: (draft, value) => {
      draft.cfg.nested_profile = value;
    },
  },
  "popup-capture-exclusion": bool("popup", "exclude_from_capture"),
  "popup-layout": choice("popup", "layout_mode", LAYOUTS),
  "popup-dictionary-styling": bool("popup", "dictionary_styling"),
  "popup-examples": bool("popup", "show_examples"),
  "popup-attributions": bool("popup", "show_attributions"),
  "popup-images": bool("popup", "show_images"),
  "popup-part-of-speech": bool("popup", "show_part_of_speech"),
  "background-on-close": {
    kind: "bool",
    get: (draft) => draft.background_on_close ?? draft.cfg.application["background-on-close"],
    set: (draft, value) => {
      draft.background_on_close = value;
    },
  },
  "lookup-mode": {
    kind: "bool",
    get: (draft) => draft.cfg.trigger.mode === "live",
    set: (draft, value) => {
      draft.cfg.trigger.mode = value ? "live" : "press";
    },
  },
  "ocr-sentence-search": {
    kind: "bool",
    get: (draft) => draft.cfg.actions.ocr_clipboard?.open_sentence_search ?? false,
    set: (draft, value) => {
      const action = draft.cfg.actions.ocr_clipboard;
      if (action) action.open_sentence_search = value;
      else if (value) draft.cfg.actions.ocr_clipboard = { open_sentence_search: true };
    },
  },
  "selected-text-sentence-search": bool("actions.search", "selected_opens_sentence_search"),
  "dictionary-terms": {
    kind: "list",
    libraryNote: true,
    get: (draft) => draft.terms,
    set: (draft, rows) => {
      draft.terms = rows;
    },
  },
  "dictionary-pitch": {
    kind: "list",
    get: (draft) => draft.pitch,
    set: (draft, rows) => {
      draft.pitch = rows;
    },
  },
  "ocr-passes": range("ocr", "max_ocr_passes", { min: 1, max: 5, step: 1 }, ""),
  "ocr-capture-size": {
    kind: "number-free",
    parts: [sizePart("Width (px)", "capture_width"), sizePart("Height (px)", "capture_height")],
  },
  "ocr-prefer-vertical": bool("ocr", "prefer_vertical"),
  "ocr-scan-alphanumeric": bool("ocr", "scan_alphanumeric"),
  "ocr-discard-furigana": bool("ocr", "discard_furigana"),
  "ocr-per-character": {
    ...bool("trigger", "per_character_lookup"),
    enabled: (draft) => draft.cfg.trigger.mode === "live",
  },
  "anki-enabled": bool("anki", "enabled"),
  "anki-notify-on-add": bool("anki", "notify_on_add"),
  "anki-overwrite-duplicates": bool("anki", "overwrite_duplicates"),
  "anki-url": { kind: "text", ...at("anki", "url") },
  "anki-deck": suggest("deck", "decks"),
  "anki-model": suggest("model", "models"),
  "anki-refresh": { kind: "action", run: refreshAnki, note: ankiNote },
  "anki-include-screenshot": bool("actions.screenshot", "include_on_add"),
  "screenshot-targets": {
    kind: "screenshot",
    options: SCREENSHOT_MODES,
    ...at("actions.screenshot", "capture_mode"),
  },
  "anki-include-dictionary-name": bool("anki", "include_dictionary_name"),
  "anki-first-dictionary-only": bool("anki", "first_dict_only"),
  "anki-selection-buttons": choice("anki", "selection_buttons", SELECTION_BUTTONS),
  "anki-selection-separator": choice("anki", "selection_separator", SELECTION_SEPARATORS),
  "anki-triple-click": choice("anki", "triple_click", TRIPLE_CLICKS),
  "anki-sentence-mode": choice("anki", "sentence_mode", SENTENCE_MODES),
  "anki-static-overlay": bool("anki", "show_static_overlay"),
  "anki-field-map": {
    kind: "field-map",
    get: (draft) => draft.field_map ?? draft.cfg.anki.field_map,
    set: (draft, map) => {
      draft.field_map = map;
    },
  },
  "debug-capture-outline": bool("debug", "show_scan_region"),
  "debug-engine": bool("debug", "show_engine_log"),
  "debug-adapter": bool("debug", "show_adapter_log"),
};

// `draft` is the `FormEdits` the editors edit.
// `anki` holds the last Anki refresh.
const state = {
  layout: null,
  fonts: [],
  form: null,
  draft: null,
  tab: null,
  anki: null,
  busy: false,
};

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function clearError() {
  const box = document.getElementById("error");
  box.hidden = true;
  box.replaceChildren();
}

function showError(error) {
  const box = document.getElementById("error");
  const kind = error && error.kind ? error.kind : "internal";
  const message = error && error.message ? error.message : String(error);
  box.replaceChildren(element("span", null, `${kind}: ${message}`));
  if (kind === "stale") {
    const reload = element("button", "action", "Reload");
    reload.addEventListener("click", reloadForm);
    box.append(reload);
  }
  box.hidden = false;
}

function setStatus(text) {
  document.getElementById("status").textContent = text;
}

function valueText(entry) {
  const read = VALUES[entry.id];
  return read ? read(state.form) : "Not shown in this window yet";
}

function controlId(entry) {
  return `control-${entry.id}`;
}

function partId(entry, index) {
  return index === 0 ? controlId(entry) : `${controlId(entry)}-${index}`;
}

function selectOf(id, options, current) {
  const select = element("select", "control");
  select.id = id;
  for (const [value, label] of options) {
    const option = element("option", null, label);
    option.value = value;
    select.append(option);
  }
  select.value = current;
  return select;
}

function withCurrent(options, current) {
  if (options.some(([value]) => value === current)) return options;
  return [...options, [current, current]];
}

function numberChoices(editor, current) {
  const values = [];
  for (let n = editor.min; n <= editor.max; n += editor.step) values.push(n);
  if (!values.includes(current)) values.push(current);
  values.sort((a, b) => a - b);
  return values.map((n) => [String(n), `${n}${editor.unit}`]);
}

function fontChoices(fonts, current) {
  const options = fonts.map((name) => [name, name]);
  if (!fonts.includes(current)) {
    options.unshift([current, `${current} (not installed)`]);
  }
  return options;
}

function profileChoices(current) {
  const options = [["", "Not set"], ...state.form.profiles.map((p) => [p.id, p.name])];
  return withCurrent(options, current);
}

function selectControl(entry, editor, options, current, convert) {
  const select = selectOf(controlId(entry), options, current);
  select.addEventListener("change", () => editor.set(state.draft, convert(select.value)));
  return select;
}

function boolControl(entry, editor) {
  const box = element("input", "control");
  box.type = "checkbox";
  box.id = controlId(entry);
  box.checked = editor.get(state.draft);
  if (editor.enabled) box.disabled = !editor.enabled(state.draft);
  box.addEventListener("change", () => editor.set(state.draft, box.checked));
  return box;
}

function textControl(entry, editor) {
  const input = element("input", "control wide-input");
  input.type = "text";
  input.id = controlId(entry);
  input.value = editor.get(state.draft);
  input.addEventListener("input", () => editor.set(state.draft, input.value));
  return input;
}

function suggestControl(entry, editor) {
  const input = textControl(entry, editor);
  const list = element("datalist");
  list.id = `list-${entry.id}`;
  input.setAttribute("list", list.id);
  for (const name of state.anki ? state.anki[editor.source] : []) {
    const option = element("option");
    option.value = name;
    list.append(option);
  }
  const group = element("span", "control-group");
  group.append(input, list);
  return group;
}

function clampInt(value) {
  return Math.max(-INT_LIMIT, Math.min(INT_LIMIT, Math.round(value)));
}

function freeNumberControl(entry, editor) {
  const group = element("div", "control-group");
  editor.parts.forEach((part, index) => {
    const input = element("input", "control number-input");
    input.type = "number";
    input.id = partId(entry, index);
    input.value = String(part.get(state.draft));
    input.setAttribute("aria-label", part.label);
    input.addEventListener("input", () => {
      const n = input.valueAsNumber;
      part.set(state.draft, Number.isFinite(n) ? clampInt(n) : part.saved(state.form));
    });
    input.addEventListener("change", () => {
      input.value = String(part.get(state.draft));
    });
    group.append(element("span", "part-label", part.label), input);
  });
  return group;
}

function swapped(rows, from, to) {
  const next = [...rows];
  [next[from], next[to]] = [next[to], next[from]];
  return next;
}

function listRow(editor, index, redraw) {
  const rows = editor.get(state.draft);
  const row = element("div", "list-row");
  const box = element("input", "control");
  box.type = "checkbox";
  box.checked = rows[index].enabled;
  box.setAttribute("aria-label", rows[index].name);
  box.addEventListener("change", () => {
    const now = editor.get(state.draft);
    editor.set(
      state.draft,
      now.map((item, position) =>
        position === index ? { ...item, enabled: box.checked } : item,
      ),
    );
  });
  row.append(box, element("span", "list-name", rows[index].name));
  for (const [text, delta] of [
    ["Move up", -1],
    ["Move down", 1],
  ]) {
    const button = element("button", "action", text);
    button.disabled = index + delta < 0 || index + delta >= rows.length;
    button.setAttribute("aria-label", `${text}: ${rows[index].name}`);
    button.addEventListener("click", () => {
      editor.set(state.draft, swapped(editor.get(state.draft), index, index + delta));
      redraw();
    });
    row.append(button);
  }
  return row;
}

function listControl(entry, editor) {
  const box = element("div", "list");
  box.id = controlId(entry);
  const redraw = () => {
    const rows = editor.get(state.draft);
    const children = rows.map((_, index) => listRow(editor, index, redraw));
    if (rows.length === 0) children.push(element("div", "entry-note", "No dictionaries."));
    if (editor.libraryNote && state.form.library_empty && rows.length > 0) {
      children.push(element("div", "entry-note", LIBRARY_NOTE));
    }
    box.replaceChildren(...children);
  };
  redraw();
  return box;
}

function withMapping(map, field, source) {
  const rest = map.filter((item) => item.anki_field !== field);
  if (source === NONE) return rest;
  const row = { anki_field: field, source };
  const index = map.findIndex((item) => item.anki_field === field);
  if (index < 0) return [...rest, row];
  return [...rest.slice(0, index), row, ...rest.slice(index)];
}

function fieldMapControl(entry, editor) {
  const map = editor.get(state.draft);
  const refreshed = state.anki && state.anki.fields.length > 0;
  const names = refreshed ? state.anki.fields : map.map((item) => item.anki_field);
  const grid = element("div", "field-map");
  if (names.length === 0) {
    grid.append(element("div", "entry-note", "No card fields yet. Reload decks and fields."));
  }
  const sources = [[NONE, NONE], ...FIELD_SOURCES.map((name) => [name, name])];
  names.forEach((name, index) => {
    const current = map.find((item) => item.anki_field === name)?.source ?? NONE;
    const select = selectOf(partId(entry, index), withCurrent(sources, current), current);
    select.addEventListener("change", () => {
      editor.set(state.draft, withMapping(editor.get(state.draft), name, select.value));
    });
    const label = element("label", "field-name", name);
    label.htmlFor = select.id;
    grid.append(label, select);
  });
  return grid;
}

function targetSummary(shot) {
  const region = shot.fixed_region;
  const window = shot.fixed_window;
  const regionText = region ? `(${region[0]}, ${region[1]}, ${region[2]}x${region[3]})` : "";
  const windowText = window
    ? `class ${JSON.stringify(window.app_id)}, title ${JSON.stringify(window.title)}`
    : "";
  if (region && window) return `Saved region: ${regionText} | window: ${windowText}`;
  if (region) return `Saved region: ${regionText}`;
  if (window) return `Saved window: ${windowText}`;
  return "No saved screenshot targets.";
}

function screenshotControl(entry, editor) {
  const group = element("div", "control-group stacked");
  const current = editor.get(state.draft);
  group.append(
    selectControl(entry, editor, withCurrent(editor.options, current), current, (v) => v),
  );
  const shot = state.draft.cfg.actions.screenshot;
  group.append(element("div", "entry-note", targetSummary(shot)));
  const reset = element("button", "action", "Clear saved targets");
  reset.disabled = !shot.fixed_region && !shot.fixed_window;
  reset.addEventListener("click", () => {
    shot.fixed_region = null;
    shot.fixed_window = null;
    state.draft.screenshot_reset_targets = true;
    render();
    setStatus("Saved screenshot targets cleared. Save or Apply to keep the change.");
  });
  group.append(reset);
  return group;
}

function actionControl(entry, editor) {
  const group = element("div", "control-group");
  const button = element("button", "action", entry.label);
  button.id = controlId(entry);
  if (editor.disabled) {
    button.disabled = true;
    button.title = "Not available in this window yet";
  } else {
    button.addEventListener("click", () => editor.run(button));
  }
  group.append(button);
  const note = editor.note ? editor.note() : "";
  if (note) group.append(element("span", "entry-note", note));
  return group;
}

function ankiNote() {
  if (!state.anki) return "";
  const { decks, models, fields } = state.anki;
  return `Loaded ${decks.length} decks, ${models.length} note types, ${fields.length} fields.`;
}

async function refreshAnki(button) {
  button.disabled = true;
  clearError();
  try {
    const { url, model } = state.draft.cfg.anki;
    state.anki = await invoke("anki_refresh", { url, model });
    render();
  } catch (error) {
    showError(error);
    button.disabled = false;
  }
}

const BUILDERS = {
  bool: boolControl,
  number: (entry, editor) => {
    const current = editor.get(state.draft);
    const options = numberChoices(editor, current);
    return selectControl(entry, editor, options, String(current), Number);
  },
  choice: (entry, editor) => {
    const current = editor.get(state.draft);
    const options = withCurrent(editor.options, current);
    return selectControl(entry, editor, options, current, (v) => v);
  },
  font: (entry, editor) => {
    const current = editor.get(state.draft);
    return selectControl(entry, editor, fontChoices(state.fonts, current), current, (v) => v);
  },
  profile: (entry, editor) => {
    const current = editor.get(state.draft) ?? "";
    const convert = (v) => (v === "" ? null : v);
    return selectControl(entry, editor, profileChoices(current), current, convert);
  },
  text: textControl,
  suggest: suggestControl,
  "number-free": freeNumberControl,
  list: listControl,
  "field-map": fieldMapControl,
  screenshot: screenshotControl,
  action: actionControl,
};

function renderEntry(entry) {
  const row = element("div", "entry");
  const editor = EDITORS[entry.id];
  if (editor) {
    const label = element("label", "entry-label", entry.label);
    label.htmlFor = controlId(entry);
    row.append(label);
    const wide = WIDE_KINDS.has(editor.kind) ? " wide" : "";
    const holder = element("div", `entry-value${wide}`);
    holder.append(BUILDERS[editor.kind](entry, editor));
    row.append(holder);
  } else {
    row.append(element("div", "entry-label", entry.label));
    row.append(element("div", "entry-value", valueText(entry)));
  }
  if (entry.help) row.append(element("div", "entry-help", entry.help));
  return row;
}

function renderSection(section) {
  const block = element("section", "section");
  block.append(element("h2", null, section.label));
  for (const entry of section.entries) {
    block.append(renderEntry(entry));
  }
  return block;
}

function renderTabs() {
  const buttons = state.layout.tabs.map((tab) => {
    const button = element("button", "tab", tab.label);
    button.setAttribute("role", "tab");
    button.setAttribute("aria-selected", String(tab.id === state.tab));
    button.addEventListener("click", () => {
      state.tab = tab.id;
      render();
      document.querySelector('#tabs [aria-selected="true"]').focus();
    });
    return button;
  });
  document.getElementById("tabs").replaceChildren(...buttons);
}

function renderPanel(tab) {
  document.getElementById("panel").replaceChildren(...tab.sections.map(renderSection));
}

function render() {
  const tab = state.layout.tabs.find((candidate) => candidate.id === state.tab);
  if (!tab) throw { kind: "parse", message: `layout has no ${state.tab} tab` };
  renderTabs();
  renderPanel(tab);
}

function draftOf(form) {
  return {
    generation: form.generation,
    cfg: structuredClone(form.cfg),
    terms: structuredClone(form.terms),
    frequency: structuredClone(form.frequency),
    pitch: structuredClone(form.pitch),
    background_on_close: form.background_on_close,
    field_map: null,
    screenshot_reset_targets: false,
  };
}

function adopt(form) {
  state.form = form;
  state.draft = draftOf(form);
  render();
}

function statusLine(report, command) {
  let line;
  if (command === "save_form") {
    line = `Settings saved to ${report.saved_to}.`;
  } else if (report.outcome === "applied" && report.reply.startsWith("ERR")) {
    line = `Settings saved. The daemon refused the reload (${report.reply}).`;
  } else if (report.outcome === "applied") {
    line = `Settings saved and daemon reloaded (${report.reply}).`;
  } else if (report.outcome === "no-reply") {
    line =
      "Settings saved. The daemon did not answer within 10 seconds. Restart it to use them.";
  } else {
    line =
      "Settings saved and will take effect next time the daemon starts (daemon is not running).";
  }
  for (const notice of report.notices) line += ` ${notice}`;
  return line;
}

function setBusy(busy) {
  state.busy = busy;
  for (const button of document.querySelectorAll("#actions button")) {
    button.disabled = busy;
  }
}

async function submit(command) {
  if (state.busy) return;
  setBusy(true);
  clearError();
  setStatus(command === "apply_form" ? "Applying..." : "Saving...");
  try {
    const report = await invoke(command, { edits: state.draft });
    adopt(report.form);
    setStatus(statusLine(report, command));
  } catch (error) {
    setStatus("");
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function reloadForm() {
  clearError();
  setStatus("");
  try {
    adopt(await invoke("load_form"));
  } catch (error) {
    showError(error);
  }
}

function renderActions() {
  const save = element("button", "action", "Save");
  save.addEventListener("click", () => submit("save_form"));
  const apply = element("button", "action primary", "Apply");
  apply.addEventListener("click", () => submit("apply_form"));
  document.getElementById("actions").replaceChildren(save, apply);
}

async function main() {
  try {
    state.layout = await invoke("load_layout");
    const first = state.layout.tabs[0];
    state.tab = first ? first.id : null;
    const form = await invoke("load_form");
    state.fonts = await invoke("font_families");
    adopt(form);
    renderActions();
  } catch (error) {
    showError(error);
  }
}

main();
