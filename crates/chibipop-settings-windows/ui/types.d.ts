// Documentation only. No compiler reads this file in stage 1.
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
  // Other sections exist. Stage 1 reads none of them.
  [section: string]: unknown;
}

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
}

interface ApplyReport {
  saved_to: string;
  notices: string[];
  form: FormView;
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
  // Stage 1 rejects both with kind "read-only".
  save_form: { args: { edits: FormEdits }; result: ApplyReport };
  apply_form: { args: { edits: FormEdits }; result: ApplyReport };
}
