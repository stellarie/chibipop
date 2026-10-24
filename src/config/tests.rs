use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("chibipop_profiles_{}_{}.toml", std::process::id(), name))
}

fn full(config: &mut Config) -> &mut ProfileSettings {
    let ProfileData::Full { settings } = &mut config.profiles[0].data else { panic!("full profile") };
    settings
}

fn derived(config: &mut Config, id: &str) {
    config.profiles.push(Profile { id: id.to_string(), name: id.to_string(),
        data: ProfileData::Derived { parent: "default".to_string(), overrides: BTreeMap::new() } });
}

fn installed() -> [crate::present::DictInfo; 2] {
    [crate::present::DictInfo { dict_id: 1, name: "大辞林　第四版".to_string() },
        crate::present::DictInfo { dict_id: 2, name: "Jitendex".to_string() }]
}

#[test]
fn full_and_derived_profiles_preserve_explicit_overrides_after_parent_edits() {
    let mut config = Config::default();
    full(&mut config).ocr.language = "ja".to_string();
    full(&mut config).popup.theme = "light".to_string();
    full(&mut config).dictionaries.terms.enabled = vec!["大辞林　第四版".to_string()];
    derived(&mut config, "bilingual");
    let mut edited = config.resolve("bilingual").unwrap();
    edited.dictionaries.terms.enabled = vec!["Jitendex".to_string()];
    edited.anki.deck = "Bilingual".to_string();
    config.update_profile("bilingual", &edited).unwrap();
    full(&mut config).popup.theme = "dark".to_string();
    full(&mut config).anki.deck = "Monolingual".to_string();
    full(&mut config).dictionaries.terms.enabled.push("New dictionary".to_string());
    let resolved = config.resolve("bilingual").unwrap();
    assert_eq!("dark", resolved.popup.theme);
    assert_eq!("Bilingual", resolved.anki.deck);
    assert_eq!(vec!["Jitendex"], resolved.dictionaries.terms.enabled);
    config.reset_override("bilingual", "anki.deck").unwrap();
    assert_eq!("Monolingual", config.resolve("bilingual").unwrap().anki.deck);
}

#[test]
fn binds_choose_a_profile_without_changing_the_default_and_reject_unknown_ids() {
    let mut config = Config::default();
    derived(&mut config, "alternate");
    config.binds[0].enabled = true;
    config.binds[0].profile = Some("alternate".to_string());
    let catalog = ProfileCatalog::new(&config, &[]).unwrap();
    assert_eq!("alternate", catalog.for_bind("lookup").unwrap().id());
    assert_eq!("default", catalog.session(None).unwrap().id());
    assert!(catalog.for_bind("unconfigured").is_err());
    config.binds[0].enabled = false;
    let catalog = ProfileCatalog::new(&config, &[]).unwrap();
    assert!(catalog.for_bind("lookup").is_err());
}

#[test]
fn migration_preserves_both_platforms_settings_and_all_legacy_action_binds() {
    let text = r#"
[trigger]
mode = "toggle"
trigger_key = "F2"
trigger_key_linux = "SUPER+J"
per_character_lookup = true
[popup]
font = "Meiryo"
layer = "top"
show_images = false
[dictionaries]
terms = ["大辞林　第四版", "Offline"]
terms_disabled = ["Jitendex"]
pitch = ["Pitch"]
frequency = ["Freq"]
frequency_disabled = ["Old Freq"]
ranking_strategy = "median"
[dictionaries.per_language]
ja = ["大辞林　第四版"]
[application]
background-on-close = true
[plugins]
enabled = ["meikiocr"]
[ocr]
engine = "meikiocr"
language = "ja"
discard_furigana = false
[anki]
enabled = true
deck = "Mining"
add_key = "F3"
add_key_linux = "SUPER+A"
static_region_key = "F4"
static_region_key_linux = "SUPER+R"
static_region = [-200, 80, 800, 300]
field_map = []
[actions.search]
hotkey = "Ctrl+F5"
hotkey_linux = "SUPER+F5"
sentence_hotkey = "Ctrl+F6"
sentence_hotkey_linux = "SUPER+F6"
selected_hotkey = "Ctrl+F7"
selected_hotkey_linux = "SUPER+F7"
selected_opens_sentence_search = true
[actions.ocr_clipboard]
hotkey = "F8"
hotkey_linux = "SUPER+F8"
open_sentence_search = true
[actions.screenshot]
capture_mode = "fixed-window"
fixed_window = { app_id = "reader", title = "読書" }
include_on_add = true
"#;
    let mut config: Config = toml::from_str(text).unwrap();
    config.migrate_dictionary_lists(&installed());
    let profile = config.resolved(None).unwrap();
    assert!(!config.live_lookup);
    assert_eq!(TriggerMode::Toggle, config.binds[0].mode);
    let chords: Vec<_> = config.binds.iter().map(|bind| (bind.id.as_str(), bind.windows.as_str(), bind.linux.as_str())).collect();
    assert_eq!(vec![("lookup", "F2", "SUPER+J"), ("anki-add", "F3", "SUPER+A"),
        ("static-region", "F4", "SUPER+R"), ("ocr-clipboard", "F8", "SUPER+F8"),
        ("search", "Ctrl+F5", "SUPER+F5"), ("sentence-search", "Ctrl+F6", "SUPER+F6"),
        ("selected-text", "Ctrl+F7", "SUPER+F7")], chords);
    assert_eq!("Meiryo", profile.popup.font);
    assert_eq!(PopupLayer::Top, profile.popup.layer);
    assert!(!profile.popup.show_images);
    assert!(profile.trigger.per_character_lookup);
    assert_eq!("meikiocr", profile.ocr.engine);
    assert!(!profile.ocr.discard_furigana);
    assert_eq!("Mining", profile.anki.deck);
    assert!(profile.anki.field_map.is_empty());
    assert_eq!(Some([-200, 80, 800, 300]), profile.anki.static_region);
    assert!(profile.actions.search.selected_opens_sentence_search);
    assert!(profile.actions.ocr_clipboard.as_ref().unwrap().open_sentence_search);
    assert_eq!("reader", profile.actions.screenshot.fixed_window.as_ref().unwrap().app_id);
    assert_eq!(vec!["Freq", "大辞林　第四版", "Jitendex"], config.dictionaries.frequency);
    assert_eq!(crate::dict::frequency::RankingStrategy::Median, config.dictionaries.ranking_strategy);
    let path = tmp("migrated");
    config.save(&path).unwrap();
    let loaded = load_or_create(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(config, loaded);
    assert_eq!(profile, loaded.resolved(None).unwrap());
}

#[test]
fn missing_and_malformed_files_have_distinct_results() {
    let path = tmp("missing");
    let _ = std::fs::remove_file(&path);
    let created = load_or_create(&path).unwrap();
    assert_eq!(created, load_or_create(&path).unwrap());
    std::fs::write(&path, "this is not = = valid toml [[[").unwrap();
    let error = load_or_create(&path).unwrap_err();
    assert!(format!("{error:#}").contains(path.to_str().unwrap()));
    assert_eq!("this is not = = valid toml [[[", std::fs::read_to_string(&path).unwrap());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn profile_load_clamps_explicit_values_without_overriding_inherited_values() {
    let mut config = Config::default();
    full(&mut config).popup.max_width_percent = 0;
    full(&mut config).popup.max_height_percent = 0;
    full(&mut config).popup.summary_chars = 5000;
    derived(&mut config, "child");
    let mut child = config.resolve("child").unwrap();
    child.ocr.capture_height = 9000;
    config.update_profile("child", &child).unwrap();
    let path = tmp("clamped");
    config.save(&path).unwrap();
    let loaded = load_or_create(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let resolved = loaded.resolve("child").unwrap();
    assert_eq!(MAX_WIDTH_RANGE.0, resolved.popup.max_width_percent);
    assert_eq!(MAX_HEIGHT_RANGE.0, resolved.popup.max_height_percent);
    assert_eq!(SUMMARY_RANGE.1, resolved.popup.summary_chars);
    assert_eq!(CAPTURE_H_RANGE.1, resolved.ocr.capture_height);
    let ProfileData::Derived { overrides, .. } = &loaded.profiles[1].data else { unreachable!() };
    assert_eq!(vec!["ocr.capture_height"], overrides.keys().map(String::as_str).collect::<Vec<_>>());
}
    // Tests for the plugin engine.
