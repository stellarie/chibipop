"use strict";

const { invoke } = window.__TAURI__.core;

const SHOWN_TAB = "popup";

const onOff = (value) => (value ? "On" : "Off");

const VALUES = {
  "popup-theme": (f) => f.cfg.popup.theme,
  "popup-font": (f) => f.cfg.popup.font,
  "popup-max-width": (f) => `${f.cfg.popup.max_width_percent}%`,
  "popup-max-height": (f) => `${f.cfg.popup.max_height_percent}%`,
  "popup-summary-length": (f) => `${f.cfg.popup.summary_chars} characters`,
  "popup-highlight": (f) => onOff(f.cfg.popup.highlight_match),
  "popup-scroll": (f) => onOff(f.cfg.popup.scroll_popup),
  "popup-edge-autoscroll": (f) => onOff(f.cfg.popup.edge_autoscroll),
  "popup-side-panel": (f) => onOff(f.cfg.popup.side_panel),
  "popup-hide-on-cursor-exit": (f) => onOff(f.cfg.popup.hide_on_cursor_exit),
  "popup-sub-popups": (f) => onOff(f.cfg.popup.sub_popups),
  "nested-profile": (f) => {
    const id = f.cfg.nested_profile;
    if (id === null) return "Not set";
    const profile = f.profiles.find((p) => p.id === id);
    return profile ? profile.name : id;
  },
  "popup-capture-exclusion": (f) => onOff(f.cfg.popup.exclude_from_capture),
  "popup-layout": (f) => f.cfg.popup.layout_mode,
  "popup-dictionary-styling": (f) => onOff(f.cfg.popup.dictionary_styling),
  "popup-examples": (f) => onOff(f.cfg.popup.show_examples),
  "popup-attributions": (f) => onOff(f.cfg.popup.show_attributions),
  "popup-images": (f) => onOff(f.cfg.popup.show_images),
  "popup-part-of-speech": (f) => onOff(f.cfg.popup.show_part_of_speech),
};

const MAX_WIDTH = { min: 10, max: 90, step: 5 };

const EDITORS = {
  "popup-highlight": {
    kind: "bool",
    get: (cfg) => cfg.popup.highlight_match,
    set: (cfg, value) => {
      cfg.popup.highlight_match = value;
    },
  },
  "popup-max-width": {
    kind: "number",
    ...MAX_WIDTH,
    get: (cfg) => cfg.popup.max_width_percent,
    set: (cfg, value) => {
      cfg.popup.max_width_percent = value;
    },
  },
  "popup-layout": {
    kind: "choice",
    options: [
      ["roomy", "Roomy"],
      ["compact", "Compact"],
    ],
    get: (cfg) => cfg.popup.layout_mode,
    set: (cfg, value) => {
      cfg.popup.layout_mode = value;
    },
  },
  "popup-font": {
    kind: "font",
    get: (cfg) => cfg.popup.font,
    set: (cfg, value) => {
      cfg.popup.font = value;
    },
  },
};

// `form` is the last view from the backend. `draft` is a copy of its
// whole `cfg` that the four editors change.
const state = { layout: null, fonts: [], form: null, draft: null, busy: false };

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

function valueText(entry, form, fonts) {
  const read = VALUES[entry.id];
  if (!read) return "Not shown in this window yet";
  const text = read(form);
  if (entry.id === "popup-font" && fonts.length > 0) {
    return fonts.includes(text) ? `${text} (installed)` : `${text} (not installed)`;
  }
  return text;
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

function numberChoices(editor, current) {
  const values = [];
  for (let n = editor.min; n <= editor.max; n += editor.step) values.push(n);
  if (!values.includes(current)) values.push(current);
  values.sort((a, b) => a - b);
  return values.map((n) => [String(n), `${n}%`]);
}

function fontChoices(fonts, current) {
  const options = fonts.map((name) => [name, name]);
  if (!fonts.includes(current)) {
    options.unshift([current, `${current} (not installed)`]);
  }
  return options;
}

function editorControl(entry, editor) {
  const id = `control-${entry.id}`;
  const current = editor.get(state.draft);
  if (editor.kind === "bool") {
    const box = element("input", "control");
    box.type = "checkbox";
    box.id = id;
    box.checked = current;
    box.addEventListener("change", () => editor.set(state.draft, box.checked));
    return box;
  }
  let select;
  if (editor.kind === "number") {
    select = selectOf(id, numberChoices(editor, current), String(current));
    select.addEventListener("change", () => editor.set(state.draft, Number(select.value)));
  } else if (editor.kind === "choice") {
    select = selectOf(id, editor.options, current);
    select.addEventListener("change", () => editor.set(state.draft, select.value));
  } else {
    select = selectOf(id, fontChoices(state.fonts, current), current);
    select.addEventListener("change", () => editor.set(state.draft, select.value));
  }
  return select;
}

function renderEntry(entry, form, fonts) {
  const row = element("div", "entry");
  const editor = EDITORS[entry.id];
  if (editor) {
    const label = element("label", "entry-label", entry.label);
    label.htmlFor = `control-${entry.id}`;
    row.append(label);
    const holder = element("div", "entry-value");
    holder.append(editorControl(entry, editor));
    row.append(holder);
  } else {
    row.append(element("div", "entry-label", entry.label));
    row.append(element("div", "entry-value", valueText(entry, form, fonts)));
  }
  if (entry.help) row.append(element("div", "entry-help", entry.help));
  return row;
}

function renderSection(section, form, fonts) {
  const block = element("section", "section");
  block.append(element("h2", null, section.label));
  for (const entry of section.entries) {
    block.append(renderEntry(entry, form, fonts));
  }
  return block;
}

function renderTab(tab, form, fonts) {
  const strip = document.getElementById("tabs");
  const button = element("button", "tab", tab.label);
  button.setAttribute("role", "tab");
  button.setAttribute("aria-selected", "true");
  strip.replaceChildren(button);

  const panel = document.getElementById("panel");
  panel.replaceChildren(
    ...tab.sections.map((section) => renderSection(section, form, fonts)),
  );
}

function render() {
  const tab = state.layout.tabs.find((candidate) => candidate.id === SHOWN_TAB);
  if (!tab) throw { kind: "parse", message: `layout has no ${SHOWN_TAB} tab` };
  renderTab(tab, state.form, state.fonts);
}

function adopt(form) {
  state.form = form;
  state.draft = structuredClone(form.cfg);
  render();
}

// The whole `cfg` that `load_form` sent, with the four editors applied.
function editsOf() {
  const form = state.form;
  return {
    generation: form.generation,
    cfg: state.draft,
    terms: form.terms,
    frequency: form.frequency,
    pitch: form.pitch,
    background_on_close: form.background_on_close,
  };
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
    const report = await invoke(command, { edits: editsOf() });
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
    const form = await invoke("load_form");
    state.fonts = await invoke("font_families");
    adopt(form);
    renderActions();
  } catch (error) {
    showError(error);
  }
}

main();
