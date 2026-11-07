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
const WIDE_KINDS = new Set(["list", "field-map", "screenshot", "binds", "catalog", "plugins"]);
const ENGINE_BUILTIN = "Built-in (Windows OCR)";
const DAEMON_NOTE = "The chibipop daemon owns this. This window cannot do it yet.";
const daemonButton = () => ({ kind: "action", disabled: true, note: () => DAEMON_NOTE });
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

const BIND_ACTIONS = [
  ["lookup", "Screen lookup"],
  ["selected-text", "Selected text"],
  ["search", "Search"],
  ["sentence-search", "Sentence search"],
  ["ocr-clipboard", "OCR to clipboard"],
  ["anki-add", "Anki add"],
  ["static-region", "Set sentence area"],
];
const NO_PROFILE_ACTIONS = new Set(["anki-add", "static-region"]);
const LOOKUP_MODES = [
  ["hold-key", "Hold"],
  ["toggle", "Toggle"],
];
const MODIFIER_NAMES = {
  ctrl: "Ctrl",
  control: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  win: "Win",
  super: "Win",
};
const KEY_NAMES = { 0x09: "Tab", 0x14: "CapsLock", 0x20: "Space", 0x1b: "Esc" };
const CAPTURE_HINT =
  "Press a key with Ctrl, Shift, or Alt. Escape cancels. Backspace clears. " +
  "Capture ends after 10 seconds.";
const REJECTED_HINT =
  "Use a key with Ctrl, Shift, or Alt. Escape cancels capture. Backspace clears.";

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
  "profile-catalog": { kind: "catalog" },
  "open-dictionary-search": daemonButton(),
  "open-sentence-search": daemonButton(),
  "show-live-logs": daemonButton(),
  "clear-lookup-cache": daemonButton(),
  "ocr-engine": { kind: "engine", ...at("ocr", "engine") },
  "ocr-language": { kind: "language", ...at("ocr", "language") },
  "plugin-list": {
    kind: "plugins",
    get: (draft) => draft.cfg.plugins.enabled,
    set: (draft, names) => {
      draft.cfg.plugins.enabled = names;
    },
  },
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
  "configured-binds": {
    kind: "binds",
    get: (draft) => draft.binds,
    set: (draft, rows) => {
      draft.binds = rows;
    },
  },
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
// `options` holds the plugin and OCR language lists for the loaded form.
const state = {
  options: { plugins: null, languages: null, languagesError: "" },
  profileName: "",
  layout: null,
  fonts: [],
  form: null,
  draft: null,
  tab: null,
  anki: null,
  busy: false,
  capturing: null,
  bindMessage: "",
  bindCheck: 0,
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
    button.title = "Not available in this window";
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

function keyLabel(part) {
  const trimmed = part.trim();
  const lower = trimmed.toLowerCase();
  if (MODIFIER_NAMES[lower]) return MODIFIER_NAMES[lower];
  if (/^f([1-9]|1[0-9]|2[0-4])$/.test(lower)) return lower.toUpperCase();
  const code = /^0x[0-9a-f]+$/.test(lower) ? Number.parseInt(lower, 16) : -1;
  if (KEY_NAMES[code]) return KEY_NAMES[code];
  return trimmed.length === 1 ? trimmed.toUpperCase() : trimmed;
}

function chordLabel(chord) {
  return chord ? chord.split("+").map(keyLabel).join("+") : "Not set";
}

function setBindMessage(text) {
  state.bindMessage = text;
  const node = document.getElementById("bind-status");
  if (node) node.textContent = text;
}

// Apply repeats this check, so a failed check never blocks a save.
async function checkBinds() {
  state.bindCheck += 1;
  const mine = state.bindCheck;
  let message;
  try {
    await invoke("validate_form", { edits: state.draft });
    message = "Shortcut check passed.";
  } catch (error) {
    message =
      error && error.kind === "validation"
        ? `Settings check failed: ${error.message}`
        : "Shortcut check unavailable.";
  }
  if (mine === state.bindCheck) setBindMessage(message);
}

function cancelCapture() {
  if (!state.capturing) return;
  invoke("cancel_capture").catch(showError);
}

document.addEventListener(
  "pointerdown",
  (event) => {
    const inside = event.target instanceof Element && event.target.closest("[data-capturing]");
    if (!inside) cancelCapture();
  },
  true,
);
window.addEventListener("blur", cancelCapture);

function bindSelect(label, options, current, onChange) {
  const select = element("select", "control");
  for (const [value, text] of withCurrent(options, current)) {
    const option = element("option", null, text);
    option.value = value;
    select.append(option);
  }
  select.value = current;
  select.setAttribute("aria-label", label);
  select.addEventListener("change", () => onChange(select.value));
  return select;
}

function bindsControl(entry, editor) {
  const box = element("div", "binds");
  box.id = controlId(entry);

  const edit = (id, change) => {
    const bind = editor.get(state.draft).find((item) => item.id === id);
    if (!bind) return;
    change(bind);
    redraw();
    checkBinds();
  };

  const capture = async (id) => {
    if (state.capturing) return;
    state.capturing = id;
    clearError();
    redraw();
    setBindMessage(CAPTURE_HINT);
    let result;
    try {
      result = await invoke("capture_chord");
    } catch (error) {
      state.capturing = null;
      redraw();
      setBindMessage("");
      showError(error);
      return;
    }
    state.capturing = null;
    setBindMessage("");
    if (result.kind === "chord") {
      edit(id, (bind) => {
        bind.windows = result.chord;
      });
      return;
    }
    if (result.kind === "cleared") {
      edit(id, (bind) => {
        bind.windows = "";
      });
      return;
    }
    redraw();
    if (result.kind === "rejected") {
      setBindMessage(`${chordLabel(result.chord)} is not a valid chord. ${REJECTED_HINT}`);
    } else if (result.kind === "timed-out") {
      setBindMessage("Capture timed out. Press Capture to try again.");
    } else {
      setBindMessage("");
    }
  };

  const row = (bind) => {
    const line = element("div", "bind-row");
    const enabled = element("input", "control");
    enabled.type = "checkbox";
    enabled.checked = bind.enabled;
    enabled.setAttribute("aria-label", `Enabled: ${bind.id}`);
    enabled.addEventListener("change", () =>
      edit(bind.id, (item) => {
        item.enabled = enabled.checked;
      }),
    );
    line.append(
      enabled,
      bindSelect("Action", BIND_ACTIONS, bind.action, (action) =>
        edit(bind.id, (item) => {
          item.action = action;
          item.mode = action === "lookup" ? "hold-key" : "press";
          if (NO_PROFILE_ACTIONS.has(action)) item.profile = null;
        }),
      ),
    );
    if (bind.action === "lookup") {
      line.append(
        bindSelect("Mode", LOOKUP_MODES, bind.mode, (mode) =>
          edit(bind.id, (item) => {
            item.mode = mode;
          }),
        ),
      );
    }
    if (!NO_PROFILE_ACTIONS.has(bind.action)) {
      const current = bind.profile ?? "";
      line.append(
        bindSelect("Profile", profileChoices(current), current, (id) =>
          edit(bind.id, (item) => {
            item.profile = id === "" ? null : id;
          }),
        ),
      );
    }
    const active = state.capturing === bind.id;
    const label = active ? "Press keys…" : chordLabel(bind.windows);
    const chord = element("span", "bind-chord", label);
    const capturing = element("button", "action", "Capture");
    capturing.setAttribute("aria-label", `Capture chord: ${bind.id}`);
    if (active) capturing.dataset.capturing = "true";
    capturing.addEventListener("click", () => capture(bind.id));
    const clear = element("button", "action", "Clear");
    clear.setAttribute("aria-label", `Clear chord: ${bind.id}`);
    clear.disabled = !bind.windows;
    clear.addEventListener("click", () =>
      edit(bind.id, (item) => {
        item.windows = "";
      }),
    );
    const remove = element("button", "action", "Remove");
    remove.setAttribute("aria-label", `Remove bind: ${bind.id}`);
    remove.addEventListener("click", () => {
      const rest = editor.get(state.draft).filter((item) => item.id !== bind.id);
      editor.set(state.draft, rest);
      redraw();
      checkBinds();
    });
    line.append(chord, capturing, clear, remove);
    return line;
  };

  const add = element("button", "action", "Add bind");
  add.addEventListener("click", async () => {
    clearError();
    try {
      const bind = await invoke("new_bind");
      editor.set(state.draft, [...editor.get(state.draft), bind]);
      redraw();
      checkBinds();
    } catch (error) {
      showError(error);
    }
  });

  const status = element("div", "entry-note bind-status", state.bindMessage);
  status.id = "bind-status";

  const redraw = () => {
    const rows = editor.get(state.draft);
    const children = rows.map(row);
    if (rows.length === 0) children.push(element("div", "entry-note", "No binds."));
    children.push(add, status);
    box.replaceChildren(...children);
  };
  redraw();
  return box;
}

function errorText(error) {
  const kind = error && error.kind ? error.kind : "internal";
  const message = error && error.message ? error.message : String(error);
  return `${kind}: ${message}`;
}

async function loadOptions(form) {
  const options = { plugins: null, languages: null, languagesError: "" };
  try {
    options.plugins = await invoke("plugin_options", { engine: form.cfg.ocr.engine });
  } catch (error) {
    showError(error);
  }
  try {
    options.languages = await invoke("ocr_languages", { language: form.cfg.ocr.language });
  } catch (error) {
    options.languagesError = errorText(error);
  }
  return options;
}

// An unenabled reader never runs.
// Choosing a reader enables it.
function currentEdits() {
  const providers = state.options.plugins ? state.options.plugins.providers : [];
  const engine = state.draft.cfg.ocr.engine;
  const enabled = state.draft.cfg.plugins.enabled;
  if (providers.includes(engine) && !enabled.includes(engine)) enabled.push(engine);
  return state.draft;
}

async function profileAction(command, args, clearName) {
  if (state.busy) return;
  state.busy = true;
  clearError();
  setStatus("");
  try {
    const form = await invoke(command, { edits: currentEdits(), ...args });
    if (clearName) state.profileName = "";
    await adopt(form);
  } catch (error) {
    showError(error);
  } finally {
    state.busy = false;
  }
}

function catalogButton(text, run, disabled) {
  const button = element("button", "action", text);
  button.disabled = Boolean(disabled);
  button.addEventListener("click", run);
  return button;
}

function catalogControl(entry) {
  const form = state.form;
  const box = element("div", "catalog");
  const nameOf = (id) => form.profiles.find((profile) => profile.id === id)?.name ?? id;
  const rows = form.profiles.map((profile) => [
    profile.id,
    profile.id === form.default_profile ? `${profile.name} (default)` : profile.name,
  ]);
  const select = selectOf(controlId(entry), rows, form.profile_id);
  select.addEventListener("change", () => profileAction("select_profile", { id: select.value }));
  const selected = form.profiles.find((profile) => profile.id === form.profile_id);
  const kind = selected && selected.parent ? `Derived from ${nameOf(selected.parent)}.` : "Full profile.";
  const manage = element("div", "control-group");
  manage.append(
    catalogButton(
      "Set as default",
      () => profileAction("set_default_profile", { id: form.profile_id }),
      form.profile_id === form.default_profile,
    ),
    catalogButton("Delete", () => profileAction("delete_profile", { id: form.profile_id })),
  );
  box.append(select, element("div", "entry-note", kind), manage);

  const name = element("input", "control wide-input");
  name.type = "text";
  name.value = state.profileName;
  name.placeholder = "Profile name";
  name.setAttribute("aria-label", "Profile name");
  name.addEventListener("input", () => {
    state.profileName = name.value;
  });
  const fulls = form.profiles.filter((profile) => !profile.parent);
  const fallback = fulls.length > 0 ? fulls[0].id : "";
  const inherited = fulls.some((profile) => profile.id === form.profile_id)
    ? form.profile_id
    : fallback;
  const parent = selectOf(
    `${controlId(entry)}-parent`,
    fulls.map((profile) => [profile.id, profile.name]),
    inherited,
  );
  parent.setAttribute("aria-label", "Parent profile");
  const named = (command, extra) => () =>
    profileAction(command, { name: state.profileName, ...extra }, true);
  const make = element("div", "control-group");
  make.append(
    name,
    catalogButton("Create", named("create_profile", { parent: null })),
    parent,
    catalogButton("Create derived", () =>
      named("create_profile", { parent: parent.value })(),
    ),
    catalogButton("Duplicate", named("duplicate_profile", {})),
    catalogButton("Rename", named("rename_profile", {})),
  );
  box.append(make);

  if (form.overrides.length > 0) {
    box.append(element("div", "entry-note", "Settings that differ from the parent:"));
    const resets = element("div", "control-group");
    for (const path of form.overrides) {
      const label = path.split(".").pop().replaceAll("_", " ");
      const reset = catalogButton(`Reset ${label}`, () =>
        profileAction("reset_profile_override", { path }),
      );
      reset.title = path;
      resets.append(reset);
    }
    box.append(resets);
  }
  return box;
}

function syncLanguage() {
  const select = document.getElementById("control-ocr-language");
  if (select) select.disabled = state.draft.cfg.ocr.engine !== "builtin";
}

function engineControl(entry, editor) {
  const current = editor.get(state.draft);
  const names = state.options.plugins ? state.options.plugins.engines : ["builtin"];
  const rows = names.map((name) => [name, name === "builtin" ? ENGINE_BUILTIN : name]);
  const select = selectControl(entry, editor, withCurrent(rows, current), current, (v) => v);
  select.addEventListener("change", syncLanguage);
  return select;
}

function languageControl(entry, editor) {
  const group = element("div", "control-group stacked");
  const { languages, languagesError } = state.options;
  const current = editor.get(state.draft);
  const rows = languages ? languages.choices.map((choice) => [choice.tag, choice.name]) : [];
  const unchanged = current === state.form.cfg.ocr.language;
  const shown = languages && unchanged ? languages.current : current;
  const select = selectControl(entry, editor, withCurrent(rows, shown), shown, (v) => v);
  select.disabled = state.draft.cfg.ocr.engine !== "builtin";
  group.append(select);
  if (languagesError) {
    group.append(element("div", "entry-note", `Could not list OCR languages. ${languagesError}`));
  } else if (rows.length === 0) {
    group.append(element("div", "entry-note", "Windows reports no installed OCR language."));
  }
  return group;
}

function pluginsControl(entry, editor) {
  const box = element("div", "plugins");
  box.id = controlId(entry);
  const info = state.options.plugins;
  if (!info) {
    box.append(element("div", "entry-note", "Could not read the plugins folder."));
    return box;
  }
  if (info.plugins.length === 0) {
    box.append(element("div", "entry-note", `No plugins found in ${info.root}.`));
    return box;
  }
  for (const plugin of info.plugins) {
    const row = element("div", "plugin-row");
    const check = element("input", "control");
    check.type = "checkbox";
    check.checked = !plugin.refused && editor.get(state.draft).includes(plugin.key);
    check.disabled = Boolean(plugin.refused);
    check.setAttribute("aria-label", `Enable: ${plugin.label}`);
    const status = element("span", "entry-note", "");
    const show = () => {
      if (plugin.refused) status.textContent = `Refused: ${plugin.refused}`;
      else status.textContent = check.checked ? "Enabled" : "Disabled";
    };
    show();
    check.addEventListener("change", () => {
      const rest = editor.get(state.draft).filter((name) => name !== plugin.key);
      editor.set(state.draft, check.checked ? [...rest, plugin.key] : rest);
      show();
    });
    const configure = element("button", "action", "Configure");
    configure.disabled = true;
    configure.title = "Not available in this window";
    configure.setAttribute("aria-label", `Configure: ${plugin.label}`);
    const label = element("span", "list-name", plugin.roles ? `${plugin.label} - ${plugin.roles}` : plugin.label);
    row.append(check, label, status, configure);
    box.append(row);
  }
  return box;
}

const BUILDERS = {
  catalog: catalogControl,
  engine: engineControl,
  language: languageControl,
  plugins: pluginsControl,
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
  binds: bindsControl,
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
    binds: structuredClone(form.binds),
    field_map: null,
    screenshot_reset_targets: false,
  };
}

async function adopt(form) {
  state.options = await loadOptions(form);
  state.form = form;
  state.draft = draftOf(form);
  state.bindMessage = "";
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
    const report = await invoke(command, { edits: currentEdits() });
    await adopt(report.form);
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
    await adopt(await invoke("load_form"));
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
    await adopt(form);
    renderActions();
  } catch (error) {
    showError(error);
  }
}

main();
