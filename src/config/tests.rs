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
fn an_explicit_value_equal_to_the_parent_remains_an_override() {
    let mut config = Config::default();
    full(&mut config).popup.theme = "light".to_string();
    derived(&mut config, "child");
    let ProfileData::Derived { overrides, .. } = &mut config.profiles[1].data else { unreachable!() };
    overrides.insert("popup.theme".to_string(), FieldOverride::Set(toml::Value::String("light".to_string())));
    full(&mut config).popup.theme = "dark".to_string();
    assert_eq!("light", config.resolve("child").unwrap().popup.theme);
}

#[test]
fn empty_role_and_language_overrides_never_enable_installed_dictionaries() {
    let mut config = Config::default();
    full(&mut config).dictionaries.terms.enabled = vec!["大辞林　第四版".to_string()];
    full(&mut config).dictionaries.pitch.enabled = vec!["Pitch".to_string()];
    derived(&mut config, "empty");
    let mut settings = config.resolve("empty").unwrap();
    settings.dictionaries.terms.enabled.clear();
    settings.dictionaries.pitch.enabled.clear();
    config.update_profile("empty", &settings).unwrap();
    let empty = ProfileCatalog::new(&config, &installed()).unwrap().session(Some("empty")).unwrap();
    assert!(empty.present_config().terms.is_empty());
    assert!(empty.present_config().pitch.is_empty());
    full(&mut config).dictionaries.per_language.insert("ja".to_string(), Vec::new());
    let scoped = ProfileCatalog::new(&config, &installed()).unwrap().session(None).unwrap();
    assert!(scoped.present_config().terms.is_empty());
    assert_eq!(vec!["Pitch"], scoped.present_config().pitch);
}

#[test]
fn named_language_and_pitch_lists_remain_independent_and_keep_unknown_names() {
    let mut config = Config::default();
    let settings = full(&mut config);
    settings.ocr.language = "zh-Hans-CN".to_string();
    settings.dictionaries.terms.enabled = vec!["Jitendex".to_string()];
    settings.dictionaries.pitch.enabled = vec!["Pitch on USB".to_string()];
    settings.dictionaries.per_language.insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
    let session = ProfileCatalog::new(&config, &installed()).unwrap().session(None).unwrap();
    let view = session.present_config();
    assert_eq!(vec!["中日大辞典"], view.terms);
    assert_eq!(vec!["Pitch on USB"], view.pitch);
    assert!(!crate::present::keeps_dict("Jitendex", &view.terms));
}

#[test]
fn retained_sessions_and_future_descendants_ignore_later_catalog_edits() {
    let mut config = Config::default();
    full(&mut config).nested_profile = Some("bilingual".to_string());
    derived(&mut config, "bilingual");
    let mut bilingual = config.resolve("bilingual").unwrap();
    bilingual.anki.deck = "Old deck".to_string();
    bilingual.nested_profile = None;
    config.update_profile("bilingual", &bilingual).unwrap();
    let catalog = ProfileCatalog::new(&config, &installed()).unwrap();
    let root = catalog.session(None).unwrap();
    bilingual.anki.deck = "New deck".to_string();
    config.update_profile("bilingual", &bilingual).unwrap();
    config.default_profile = "bilingual".to_string();
    let future = ProfileCatalog::new(&config, &installed()).unwrap().session(None).unwrap();
    assert_eq!("default", root.id());
    assert_eq!("Old deck", root.nested().config().anki.deck);
    assert_eq!("bilingual", future.id());
    assert_eq!("New deck", future.config().anki.deck);
    assert_eq!("bilingual", root.nested().nested().id());
}

#[test]
fn option_overrides_can_clear_a_parent_target_without_losing_inheritance() {
    let mut config = Config::default();
    full(&mut config).actions.screenshot.fixed_region = Some([20, 30, 400, 200]);
    derived(&mut config, "child");
    let mut settings = config.resolve("child").unwrap();
    settings.actions.screenshot.fixed_region = None;
    config.update_profile("child", &settings).unwrap();
    let text = toml::to_string_pretty(&config).unwrap();
    let restored: Config = toml::from_str(&text).unwrap();
    assert_eq!(None, restored.resolve("child").unwrap().actions.screenshot.fixed_region);
    assert_eq!(Some([20, 30, 400, 200]), restored.resolve("default").unwrap().actions.screenshot.fixed_region);
    config.reset_override("child", "actions.screenshot.fixed_region").unwrap();
    assert_eq!(Some([20, 30, 400, 200]), config.resolve("child").unwrap().actions.screenshot.fixed_region);
}

#[test]
fn inheritance_requires_a_full_parent_and_routing_does_not_create_inheritance() {
    let mut config = Config::default();
    derived(&mut config, "child");
    full(&mut config).nested_profile = Some("child".to_string());
    assert_eq!("child", ProfileCatalog::new(&config, &[]).unwrap().session(None).unwrap().nested().id());
    config.profiles.push(Profile { id: "grandchild".to_string(), name: "Grandchild".to_string(),
        data: ProfileData::Derived { parent: "child".to_string(), overrides: BTreeMap::new() } });
    assert!(config.validate().is_err());
}

#[test]
fn profile_references_block_deletion_but_do_not_rewrite_a_retained_session() {
    let mut config = Config::default();
    derived(&mut config, "child");
    full(&mut config).nested_profile = Some("child".to_string());
    config.binds[0].profile = Some("child".to_string());
    let retained = ProfileCatalog::new(&config, &[]).unwrap().session(Some("child")).unwrap();
    assert!(config.remove_profile("default").is_err());
    assert!(config.remove_profile("child").is_err());
    full(&mut config).nested_profile = None;
    config.binds[0].profile = None;
    config.remove_profile("child").unwrap();
    assert!(config.resolve("child").is_err());
    assert_eq!("child", retained.id());
    assert_eq!("child", retained.nested().id());
}

#[test]
fn stable_ids_survive_rename_and_deleted_ids_are_not_reused() {
    let mut config = Config::default();
    let id = config.next_profile_id();
    derived(&mut config, &id);
    config.binds[0].profile = Some(id.clone());
    config.profiles[1].name = "Renamed".to_string();
    assert_eq!(id, config.binds[0].profile.as_deref().unwrap());
    config.binds[0].profile = None;
    config.remove_profile(&id).unwrap();
    let text = toml::to_string(&config).unwrap();
    let mut restored: Config = toml::from_str(&text).unwrap();
    assert_ne!(id, restored.next_profile_id());
}

#[test]
fn profiles_reject_unknown_fields_wrong_types_and_broken_references() {
    let mut config = Config::default();
    derived(&mut config, "child");
    let ProfileData::Derived { overrides, .. } = &mut config.profiles[1].data else { unreachable!() };
    overrides.insert("plugins.enabled".to_string(), FieldOverride::Set(toml::Value::Array(Vec::new())));
    assert!(config.validate().is_err());
    let ProfileData::Derived { overrides, .. } = &mut config.profiles[1].data else { unreachable!() };
    overrides.clear();
    overrides.insert("popup.theme".to_string(), FieldOverride::Set(toml::Value::Boolean(true)));
    assert!(config.validate().is_err());
    let ProfileData::Derived { overrides, .. } = &mut config.profiles[1].data else { unreachable!() };
    overrides.clear();
    full(&mut config).nested_profile = Some("missing".to_string());
    assert!(config.validate().is_err());
    full(&mut config).nested_profile = None;
    config.default_profile = "missing".to_string();
    assert!(config.validate().is_err());
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
fn bind_modes_and_profile_overrides_apply_only_to_supported_actions() {
    let mut config = Config::default();
    config.binds[1].mode = TriggerMode::Toggle;
    assert!(config.validate().is_err());
    config.binds[1].mode = TriggerMode::Press;
    config.binds[1].profile = Some("default".to_string());
    assert!(config.validate().is_err());
    config.binds[1].profile = None;
    config.binds[0].mode = TriggerMode::Live;
    assert!(config.validate().is_err());
}

#[test]
fn shortcut_validation_handles_aliases_chords_escape_and_disabled_binds() {
    let mut config = Config::default();
    config.binds[0].enabled = true;
    config.binds[1].enabled = true;
    config.binds[0].windows = "Ctrl+Shift+F5".to_string();
    config.binds[1].windows = " shift + control + f5 ".to_string();
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[1].windows = "Alt+F5".to_string();
    config.validate_hotkeys(Platform::Windows).unwrap();
    config.binds[1].windows = "F5".to_string();
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[1].enabled = false;
    config.validate_hotkeys(Platform::Windows).unwrap();
    config.binds[0].windows = "0x1B".to_string();
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[0].windows = "not a key".to_string();
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[1].enabled = true;
    config.binds[0].linux = "Control+Super+F2".to_string();
    config.binds[1].linux = " win + ctrl + f2 ".to_string();
    assert!(config.validate_hotkeys(Platform::Linux).is_err());
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
fn legacy_substrings_and_empty_language_lists_migrate_before_explicit_scopes() {
    let text = "[trigger]\nmode = 'hold-shift'\n[popup]\n[dictionaries]\ndisplay_order = ['Jiten', 'Missing']\n[dictionaries.per_language]\nja = []\nzh = ['大辞']\n";
    let config: Config = toml::from_str(text).unwrap();
    let catalog = ProfileCatalog::new(&config, &installed()).unwrap();
    assert_eq!(TriggerMode::HoldKey, catalog.config.binds[0].mode);
    let profile = catalog.session(None).unwrap();
    let present = profile.present_config();
    assert_eq!(vec!["Jitendex", "大辞林　第四版"], present.terms);
    assert_eq!(vec!["大辞林　第四版"], profile.config().dictionaries.per_language["zh"]);
    assert!(!profile.config().dictionaries.per_language.contains_key("ja"));
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
    #[test]
    fn parse_trigger_key_shift() {
        assert_eq!(Some(0x10), parse_trigger_key("shift"));
    }

    #[test]
    fn parse_trigger_key_case_insensitive() {
        assert_eq!(Some(0x10), parse_trigger_key("SHIFT"));
        assert_eq!(Some(0x10), parse_trigger_key("Shift"));
    }

    #[test]
    fn parse_trigger_key_ctrl() {
        assert_eq!(Some(0x11), parse_trigger_key("ctrl"));
        assert_eq!(Some(0x11), parse_trigger_key("control"));
    }

    #[test]
    fn parse_trigger_key_alt() {
        assert_eq!(Some(0x12), parse_trigger_key("alt"));
    }

    #[test]
    fn parse_trigger_key_f1() {
        assert_eq!(Some(0x70), parse_trigger_key("f1"));
    }

    #[test]
    fn parse_trigger_key_f12() {
        assert_eq!(Some(0x7B), parse_trigger_key("f12"));
    }

    #[test]
    fn parse_trigger_key_lowercase_letter() {
        assert_eq!(Some(0x41), parse_trigger_key("a"));
    }

    #[test]
    fn parse_trigger_key_uppercase_letter() {
        assert_eq!(Some(0x41), parse_trigger_key("A"));
    }

    #[test]
    fn parse_trigger_key_digit_key() {
        assert_eq!(Some(0x35), parse_trigger_key("5"));
    }

    /// Confirms that the parser and display use the same key names.
    #[test]
    fn parse_trigger_key_single_char_matches_trigger_key_name() {
        for c in 'a'..='z' {
            let vk = parse_trigger_key(&c.to_string()).unwrap();
            assert_eq!(c.to_ascii_uppercase().to_string(), trigger_key_name(vk));
        }
        for c in '0'..='9' {
            let vk = parse_trigger_key(&c.to_string()).unwrap();
            assert_eq!(c.to_string(), trigger_key_name(vk));
        }
    }

    #[test]
    fn parse_trigger_key_garbage() {
        assert_eq!(None, parse_trigger_key("garbage"));
    }

    #[test]
    fn parse_trigger_key_hex() {
        assert_eq!(Some(0x41), parse_trigger_key("0x41"));
    }

    #[test]
    fn parse_trigger_key_hex_uppercase_prefix() {
        assert_eq!(Some(0x41), parse_trigger_key("0X41"));
    }

    #[test]
    fn parse_trigger_key_decimal() {
        assert_eq!(Some(0x41), parse_trigger_key("65"));
    }

    /// The value overflows `u16`. The parser rejects it and does not wrap it.
    #[test]
    fn parse_trigger_key_out_of_range_decimal_is_rejected() {
        assert_eq!(None, parse_trigger_key("99999"));
    }

    #[test]
    fn trigger_key_name_round_trips() {
        for (name, want) in &[
            ("shift", "Shift"), ("ctrl", "Ctrl"), ("alt", "Alt"),
            ("f1", "F1"), ("f2", "F2"), ("f3", "F3"), ("f4", "F4"),
            ("f5", "F5"), ("f6", "F6"), ("f7", "F7"), ("f8", "F8"),
            ("f9", "F9"), ("f10", "F10"), ("f11", "F11"), ("f12", "F12"),
        ] {
            let vk = parse_trigger_key(name).unwrap();
            assert_eq!(*want, trigger_key_name(vk));
        }
    }

    #[test]
    fn trigger_key_name_letter() {
        assert_eq!("A", trigger_key_name(0x41));
    }

    #[test]
    fn trigger_key_name_digit() {
        assert_eq!("5", trigger_key_name(0x35));
    }

    #[test]
    fn trigger_key_name_space() {
        assert_eq!("Space", trigger_key_name(0x20));
    }

    #[test]
    fn trigger_key_name_named_specials() {
        assert_eq!("Esc", trigger_key_name(0x1B));
        assert_eq!("Tab", trigger_key_name(0x09));
        assert_eq!("CapsLock", trigger_key_name(0x14));
    }

    #[test]
    fn trigger_key_name_unknown_falls_back_to_hex() {
        assert_eq!("Key 0xBA", trigger_key_name(0xBA));
    }
    #[test]
    fn capture_values_below_the_floor_are_clamped_up() {
        let mut c = ResolvedConfig::default();
        c.ocr.capture_width = 1;
        c.ocr.capture_height = 1;
        c.clamp_ranges(Some(Path::new("test.toml")));
        assert_eq!(CAPTURE_W_RANGE.0, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.0, c.ocr.capture_height);
    }

    #[test]
    fn capture_values_above_the_ceiling_are_clamped_down() {
        let mut c = ResolvedConfig::default();
        c.ocr.capture_width = 99_999;
        c.ocr.capture_height = 99_999;
        c.clamp_ranges(Some(Path::new("test.toml")));
        assert_eq!(CAPTURE_W_RANGE.1, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.1, c.ocr.capture_height);
    }

    /// Confirms that both boundary values are valid.
    #[test]
    fn capture_values_exactly_on_the_bounds_are_untouched() {
        let mut c = ResolvedConfig::default();
        c.ocr.capture_width = CAPTURE_W_RANGE.0;
        c.ocr.capture_height = CAPTURE_H_RANGE.1;
        c.clamp_ranges(Some(Path::new("test.toml")));
        assert_eq!(CAPTURE_W_RANGE.0, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.1, c.ocr.capture_height);
    }
    /// `popup.layer` accepts exactly `overlay` and `top`.
    #[test]
    fn popup_layer_parses_overlay_and_top_and_rejects_garbage() {
        let base = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[dictionaries]\ndisplay_order = []\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"X\"\n",
        );
        let parse = |layer_line: &str| {
            toml::from_str::<ResolvedConfig>(&format!("{base}{layer_line}"))
        };
        assert_eq!(PopupLayer::Overlay, parse("layer = \"overlay\"\n").unwrap().popup.layer);
        assert_eq!(PopupLayer::Top, parse("layer = \"top\"\n").unwrap().popup.layer);
        assert_eq!(PopupLayer::Overlay, parse("").unwrap().popup.layer, "absent takes the default");
        assert!(parse("layer = \"bottom\"\n").is_err(), "garbage layers are a parse error");
    }

    /// `popup.layout_mode` accepts exactly `roomy` and `compact`.
    ///
    /// This test matches [`PopupLayer`] and rejects an unknown enum.
    /// It does not choose a silent default.
    /// A config with an unknown mode came from a build that this build does not understand.
    #[test]
    fn layout_mode_parses_roomy_and_compact_and_rejects_garbage() {
        let base = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[dictionaries]\ndisplay_order = []\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"X\"\n",
        );
        let parse = |line: &str| toml::from_str::<ResolvedConfig>(&format!("{base}{line}"));
        let mode = |line: &str| parse(line).unwrap().popup.layout_mode;
        assert_eq!(LayoutMode::Roomy, mode("layout_mode = \"roomy\"\n"));
        assert_eq!(LayoutMode::Compact, mode("layout_mode = \"compact\"\n"));
        assert_eq!(LayoutMode::Roomy, mode(""), "absent takes the default");
        assert!(parse("layout_mode = \"terse\"\n").is_err(), "garbage modes are a parse error");
    }
    /// Confirms that the code keeps a resolvable literal unchanged.
    #[test]
    fn a_resolvable_font_is_kept() {
        let choice = resolve_font("IPAexGothic", Platform::Linux, |_| true);
        assert_eq!(FontChoice::Configured("IPAexGothic".to_string()), choice);
        assert_eq!("IPAexGothic", choice.family());
    }

    /// Confirms that an unresolvable literal uses the platform default.
    /// It also records the requested family.
    #[test]
    fn an_unresolvable_font_falls_back_to_the_platform_default() {
        let choice = resolve_font("Yu Gothic UI", Platform::Linux, |_| false);
        assert_eq!(
            FontChoice::Fallback {
                requested: "Yu Gothic UI".to_string(),
                family: "Noto Sans CJK JP",
            },
            choice
        );
        assert_eq!("Noto Sans CJK JP", choice.family());
        let windows = resolve_font("Noto Sans CJK JP", Platform::Windows, |_| false);
        assert_eq!("Yu Gothic UI", windows.family());
    }

    /// Confirms that an empty literal skips the resolver and uses the platform default.
    #[test]
    fn an_empty_font_falls_back_without_asking() {
        let choice = resolve_font("", Platform::Linux, |_| panic!("asked about an empty literal"));
        assert_eq!("Noto Sans CJK JP", choice.family());
    }

    // Tests for the plugin engine.

    #[test]
    fn an_engine_naming_a_plugin_that_is_not_enabled_falls_back() {
        let chosen = resolve_engine("manga-ocr", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::FellBack("manga-ocr".into()));
    }

    #[test]
    fn an_enabled_plugin_is_chosen() {
        let chosen = resolve_engine("meikiocr", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::Plugin("meikiocr".into()));
    }

    /// Confirms that the builtin engine ignores the plugin list.
    #[test]
    fn builtin_wins_even_with_plugins_enabled() {
        let chosen = resolve_engine("builtin", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::Builtin);
    }

    #[test]
    fn an_unknown_engine_falls_back_with_no_plugins_enabled() {
        let chosen = resolve_engine("meikiocr", &[]);
        assert_eq!(chosen, EngineChoice::FellBack("meikiocr".into()));
    }
    #[test]
    fn parse_hotkey_single_key() {
        let (vk, mods) = parse_hotkey("a").unwrap();
        assert_eq!(0x41, vk);
        assert_eq!(0, mods);
    }

    #[test]
    fn parse_hotkey_ctrl_shift_s() {
        let (vk, mods) = parse_hotkey("ctrl+shift+s").unwrap();
        assert_eq!(0x53, vk);
        assert_eq!(0b011, mods);
    }

    #[test]
    fn parse_hotkey_alt_f10() {
        let (vk, mods) = parse_hotkey("alt+f10").unwrap();
        assert_eq!(0x79, vk);
        assert_eq!(0b100, mods);
    }

    #[test]
    fn parse_hotkey_case_insensitive() {
        let (vk, mods) = parse_hotkey("Ctrl+Shift+S").unwrap();
        assert_eq!(0x53, vk);
        assert_eq!(0b011, mods);
    }

    #[test]
    fn parse_hotkey_garbage() {
        assert!(parse_hotkey("garbage+garbage").is_none());
    }

    #[test]
    fn parse_hotkey_empty() {
        assert!(parse_hotkey("").is_none());
    }

#[test]
fn bare_action_shortcuts_and_lookup_shortcuts_keep_distinct_modifier_rules() {
    let mut config = Config::default();
    config.binds.clear();
    let mut plain = Bind::new("plain".to_string(), BindAction::Search);
    plain.windows = "S".to_string();
    let mut chord = Bind::new("chord".to_string(), BindAction::SentenceSearch);
    chord.windows = "Ctrl+S".to_string();
    config.binds = vec![plain, chord];
    config.validate_hotkeys(Platform::Windows).unwrap();
    config.binds[0].action = BindAction::Lookup;
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[0].windows = "shift".to_string();
    config.binds[1].windows = "0xA1".to_string();
    assert!(config.validate_hotkeys(Platform::Windows).is_err());
    config.binds[0].windows = "0xA0".to_string();
    config.validate_hotkeys(Platform::Windows).unwrap();
}

#[test]
fn legacy_disabled_actions_keep_their_saved_shortcuts_inactive() {
    let config: Config = toml::from_str(
        "[trigger]\nmode = 'live'\n[popup]\n[dictionaries]\n[anki]\nenabled = false\nadd_key = 'F3'\n\
         sentence_mode = 'static'\nstatic_region_key = 'F4'\n[actions]\nenabled = false\n\
         [actions.search]\nhotkey = 'Ctrl+F5'\n"
    ).unwrap();
    assert!(config.binds.iter().all(|bind| !bind.enabled));
    assert_eq!("F3", config.binds.iter().find(|bind| bind.id == "anki-add").unwrap().windows);
    assert_eq!("Ctrl+F5", config.binds.iter().find(|bind| bind.id == "search").unwrap().windows);
}

#[test]
fn duplicate_and_invalid_ids_cannot_ambiguously_select_a_profile_or_bind() {
    let mut config = Config::default();
    config.profiles.push(config.profiles[0].clone());
    assert!(config.validate().is_err());
    config.profiles.pop();
    config.binds.push(config.binds[0].clone());
    assert!(config.validate().is_err());
    config.binds.pop();
    for id in ["", "two words", "../profile", "line\nbreak"] {
        config.binds[0].id = id.to_string();
        assert!(config.validate().is_err());
    }
}
