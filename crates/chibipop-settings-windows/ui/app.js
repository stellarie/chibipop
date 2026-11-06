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

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function showError(error) {
  const box = document.getElementById("error");
  const kind = error && error.kind ? error.kind : "internal";
  const message = error && error.message ? error.message : String(error);
  box.textContent = `${kind}: ${message}`;
  box.hidden = false;
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

function renderEntry(entry, form, fonts) {
  const row = element("div", "entry");
  row.append(element("div", "entry-label", entry.label));
  row.append(element("div", "entry-value", valueText(entry, form, fonts)));
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

async function main() {
  try {
    const layout = await invoke("load_layout");
    const form = await invoke("load_form");
    const fonts = await invoke("font_families");
    const tab = layout.tabs.find((candidate) => candidate.id === SHOWN_TAB);
    if (!tab) throw { kind: "parse", message: `layout has no ${SHOWN_TAB} tab` };
    renderTab(tab, form, fonts);
  } catch (error) {
    showError(error);
  }
}

main();
