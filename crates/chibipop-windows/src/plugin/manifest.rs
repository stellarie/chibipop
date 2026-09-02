//! Parse plugin manifests so the plugin host can validate protocol and role declarations.
//! This check keeps unsupported declarations out of the plugin process.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    TextProvider,
    FieldContributor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Reactive,
    Ambient,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TextProviderCfg {
    #[serde(default)]
    pub provides_geometry: bool,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default = "default_text_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldContributorCfg {
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub wants_screen_image: bool,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    #[serde(default = "default_field_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub roles: Vec<Role>,
    pub text_provider: Option<TextProviderCfg>,
    pub field_contributor: Option<FieldContributorCfg>,
}

fn default_text_timeout() -> u64 { 500 }
fn default_field_timeout() -> u64 { 1500 }
fn default_mode() -> Mode { Mode::Reactive }

pub const SUPPORTED: &[u32] = &[1];

pub fn parse(text: &str) -> Result<Manifest> {
    let m: Manifest = toml::from_str(text).context("reading plugin.toml")?;

    if !SUPPORTED.contains(&m.protocol) {
        bail!(
            "plugin \"{}\" declares protocol {}, and chibipop supports {:?}",
            m.name, m.protocol, SUPPORTED
        );
    }
    if m.roles.is_empty() {
        bail!("plugin \"{}\" declares no roles", m.name);
    }
    if m.name.trim().is_empty() {
        bail!("a plugin manifest has an empty name");
    }
    if m.roles.contains(&Role::TextProvider) && m.text_provider.is_none() {
        bail!("plugin \"{}\" claims text-provider with no [text_provider]", m.name);
    }
    if m.roles.contains(&Role::FieldContributor) {
        let cfg = m.field_contributor.as_ref().with_context(|| {
            format!("plugin \"{}\" claims field-contributor with no section", m.name)
        })?;
        if cfg.provides.is_empty() {
            bail!("plugin \"{}\" contributes no field names", m.name);
        }
        if cfg.mode == Mode::Ambient {
            bail!(
                "plugin \"{}\" wants ambient mode, which protocol 1 does not run",
                m.name
            );
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "manga-ocr"
version = "0.1.0"
protocol = 1
command = "python"
args = ["-u", "main.py"]
roles = ["text-provider"]

[text_provider]
provides_geometry = false
languages = ["ja"]
timeout_ms = 500
"#;

    #[test]
    fn a_good_manifest_parses() {
        let m = parse(GOOD).unwrap();
        assert_eq!(m.name, "manga-ocr");
        assert_eq!(m.roles, vec![Role::TextProvider]);
        let t = m.text_provider.unwrap();
        assert!(!t.provides_geometry);
        assert_eq!(t.timeout_ms, 500);
    }

    #[test]
    fn an_unsupported_protocol_is_refused_by_number() {
        let bad = GOOD.replace("protocol = 1", "protocol = 7");
        let e = parse(&bad).unwrap_err().to_string();
        assert!(e.contains('7'), "{e}");
        assert!(e.contains("[1]"), "{e}");
    }

    #[test]
    fn an_unknown_role_is_refused() {
        let bad = GOOD.replace(r#""text-provider""#, r#""sandwich-maker""#);
        assert!(parse(&bad).is_err());
    }

    #[test]
    fn a_claimed_role_without_its_section_is_refused() {
        let bad = GOOD.replace("roles = [\"text-provider\"]",
            "roles = [\"text-provider\", \"field-contributor\"]");
        let e = parse(&bad).unwrap_err().to_string();
        assert!(e.contains("field-contributor"), "{e}");
    }

    #[test]
    fn a_contributor_with_no_provides_list_is_refused() {
        let bad = format!("{GOOD}\n[field_contributor]\nprovides = []\n");
        let bad = bad.replace("roles = [\"text-provider\"]",
            "roles = [\"text-provider\", \"field-contributor\"]");
        let e = parse(&bad).unwrap_err().to_string();
        assert!(e.contains("no field names"), "{e}");
    }

    #[test]
    fn ambient_mode_is_refused_and_says_why() {
        let bad = format!(
            "{GOOD}\n[field_contributor]\nprovides = [\"x\"]\nmode = \"ambient\"\n"
        );
        let bad = bad.replace("roles = [\"text-provider\"]",
            "roles = [\"text-provider\", \"field-contributor\"]");
        let e = parse(&bad).unwrap_err().to_string();
        assert!(e.contains("ambient"), "{e}");
        assert!(e.contains("protocol 1"), "{e}");
    }

    #[test]
    fn a_reactive_contributor_parses() {
        let ok = format!("{GOOD}\n[field_contributor]\nprovides = [\"screenshot\"]\n");
        let ok = ok.replace("roles = [\"text-provider\"]",
            "roles = [\"text-provider\", \"field-contributor\"]");
        let m = parse(&ok).unwrap();
        let f = m.field_contributor.unwrap();
        assert_eq!(f.mode, Mode::Reactive);
        assert_eq!(f.timeout_ms, 1500);
        assert!(!f.wants_screen_image);
    }
}
