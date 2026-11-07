// Documentation only. No compiler reads this file.
// Each type mirrors a Rust type in `src/ipc.rs` or `src/layout.rs`.

type ErrorKind =
  | "io"
  | "parse"
  | "dictionary"
  | "validation"
  | "stale"
  | "read-only"
  | "internal";

interface IpcError {
  kind: ErrorKind;
  message: string;
}

interface ProfileRef {
  id: string;
  name: string;
}

interface DictRow {
  name: string;
  enabled: boolean;
}

// `ResolvedConfig` from the core crate, in its serialized form.
interface ResolvedConfig {
  popup: {
    sub_popups: boolean;
    theme: string;
    exclude_from_capture: boolean;
    max_width_percent: number;
    max_height_percent: number;
    summary_chars: number;
    font: string;
    highlight_match: boolean;
    scroll_popup: boolean;
    edge_autoscroll: boolean;
    side_panel: boolean;
    layer: "overlay" | "top";
    layout_mode: "roomy" | "compact";
    dictionary_styling: boolean;
    show_examples: boolean;
    show_attributions: boolean;
    show_images: boolean;
    show_part_of_speech: boolean;
    hide_on_cursor_exit: boolean;
  };
  nested_profile: string | null;
  trigger: {
    mode: "live" | "hold-key" | "toggle" | "press" | "hold-shift";
    per_character_lookup: boolean;
  };
  dictionaries: {
    ranking_strategy: "best-rank" | "priority" | "median";
  };
  // The key is kebab-case in the serialized form.
  application: { "background-on-close": boolean };
  ocr: {
    max_ocr_passes: number;
    prefer_vertical: boolean;
    capture_width: number;
    capture_height: number;
    scan_alphanumeric: boolean;
    discard_furigana: boolean;
    language: string;
    engine: string;
  };
  debug: {
    show_scan_region: boolean;
    show_engine_log: boolean;
    show_adapter_log: boolean;
  };
  anki: {
    enabled: boolean;
    overwrite_duplicates: boolean;
    url: string;
    deck: string;
    model: string;
    notify_on_add: boolean;
    field_map: FieldMapping[];
    sentence_mode: "sentence" | "line" | "all" | "static";
    show_static_overlay: boolean;
    include_dictionary_name: boolean;
    first_dict_only: boolean;
    selection_buttons: "primary-additive" | "primary-replacing";
    selection_separator: "ellipsis" | "space" | "line-break" | "list-items";
    triple_click: "sense" | "sense-with-examples" | "line";
  };
  actions: {
    screenshot: {
      include_on_add: boolean;
      capture_mode: "region" | "window" | "fixed-region" | "fixed-window";
      fixed_region: [number, number, number, number] | null;
      fixed_window: { app_id: string; title: string } | null;
    };
    ocr_clipboard: { open_sentence_search: boolean } | null;
    search: { selected_opens_sentence_search: boolean };
  };
  // Other sections exist. The page ignores them.
  [section: string]: unknown;
}

interface FieldMapping {
  anki_field: string;
  source: string;
}

type BindAction =
  | "lookup"
  | "selected-text"
  | "search"
  | "sentence-search"
  | "ocr-clipboard"
  | "anki-add"
  | "static-region";

// Mirrors `Bind` in the core crate, which denies unknown fields.
// The page edits `windows` only and returns `linux` unchanged.
interface Bind {
  id: string;
  action: BindAction;
  windows: string;
  linux: string;
  mode: "live" | "hold-key" | "toggle" | "press" | "hold-shift";
  profile: string | null;
  enabled: boolean;
}

// The result of one `capture_chord` call.
type CaptureResult =
  | { kind: "chord"; chord: string }
  | { kind: "cleared" }
  | { kind: "cancelled" }
  // `parse_hotkey` refused the chord, for example a Win chord.
  | { kind: "rejected"; chord: string }
  | { kind: "timed-out" };

interface FormView {
  profile_id: string;
  profiles: ProfileRef[];
  cfg: ResolvedConfig;
  terms: DictRow[];
  frequency: DictRow[];
  pitch: DictRow[];
  dict_list_language: string;
  unreadable: string[];
  library_empty: boolean;
  background_on_close: boolean | null;
  binds: Bind[];
  stale: string[];
  generation: number;
}

interface FormEdits {
  generation: number;
  cfg: ResolvedConfig;
  terms: DictRow[];
  frequency: DictRow[];
  pitch: DictRow[];
  background_on_close: boolean | null;
  binds: Bind[];
  // Null keeps the saved map.
  field_map: FieldMapping[] | null;
  // True clears both saved screenshot targets.
  screenshot_reset_targets: boolean;
}

interface AnkiLists {
  decks: string[];
  models: string[];
  fields: string[];
}

interface ApplyReport {
  saved_to: string;
  notices: string[];
  form: FormView;
  outcome: "saved" | "applied" | "no-reply";
  // The daemon reply line. Set only when outcome is "applied".
  reply: string | null;
}

interface EntryView {
  id: string;
  label: string;
  help: string | null;
  inline_help: boolean;
}

interface SectionView {
  id: string;
  label: string;
  entries: EntryView[];
}

interface TabView {
  id: string;
  label: string;
  sections: SectionView[];
}

interface LayoutView {
  version: number;
  tabs: TabView[];
}

// Every command rejects with an `IpcError`.
interface Commands {
  load_layout: { args: {}; result: LayoutView };
  load_form: { args: {}; result: FormView };
  font_families: { args: {}; result: string[] };
  // Both reject a reindex edit: "read-only".
  // Both reject a stale generation: "stale".
  save_form: { args: { edits: FormEdits }; result: ApplyReport };
  apply_form: { args: { edits: FormEdits }; result: ApplyReport };
  // Rejects with kind "io" when Anki is down.
  anki_refresh: { args: { url: string; model: string }; result: AnkiLists };
  // Arms a global keyboard hook for up to 10 seconds.
  // Rejects with kind "internal" when a capture already runs.
  capture_chord: { args: {}; result: CaptureResult };
  cancel_capture: { args: {}; result: void };
  // Returns a Lookup bind with a fresh id. The page appends it to `binds`.
  new_bind: { args: {}; result: Bind };
  // Dry run of a save. Writes nothing.
  // Rejects with kind "validation" for any settings problem, not only a chord conflict.
  // Rejects with kind "read-only" while an unrelated reindex edit is pending.
  validate_form: { args: { edits: FormEdits }; result: string[] };
}
