//! Rules model and per-app installation adapters.
//!
//! A rule stores its content only once (in the database). Once enabled, every app
//! installs it through the same three-step flow:
//!
//! 1. `RuleAdapter::locate`: resolve the install location (rules directory,
//!    extension, and how the app reads rules);
//! 2. `RuleAdapter::render`: produce the rule file content the app understands;
//! 3. `RuleAdapter::apply_integration`: change settings so the app reads the
//!    rules; this step is a no-op for apps that read the directory natively.
//!
//! The service layer calls these steps in a fixed order (template method); apps
//! differ only in how each step is implemented: Claude Code reads the rules
//! directory natively; Codex and Gemini CLI inject rules through a SessionStart
//! hook; OpenCode references the rules directory in the global `instructions`.
//!
//! The model keeps toggle fields and render branches for the remaining apps
//! (Antigravity, Kimi, Copilot, ...). Enabling one later only requires adding it
//! to `SUPPORTED_APPS`; `rule_support` / `rule_adapter` gate every unsupported
//! combination and never write any file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;

/// Apps wired up as rule targets. Actual availability is filtered again by
/// `rule_support` (Codex, for example, has no usable hook mechanism on Windows).
pub const SUPPORTED_APPS: [AppType; 4] = [
    AppType::Claude,
    AppType::Codex,
    AppType::Gemini,
    AppType::OpenCode,
];

/// Maximum body size for a single rule. Hook apps inject rules into context, so
/// oversized rules are both slow and eat into the context window.
pub const MAX_RULE_CONTENT_BYTES: usize = 256 * 1024;

/// Which apps a rule is enabled in. Field order matches the frontend matrix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleApps {
    pub claude: bool,
    pub codex: bool,
    pub gemini: bool,
    pub grokbuild: bool,
    pub opencode: bool,
    pub hermes: bool,
    pub pi: bool,
    pub mcode: bool,
    pub kimi: bool,
    pub dsh: bool,
    pub agy: bool,
    pub omp: bool,
    pub copilot: bool,
    pub workbuddy: bool,
}

impl RuleApps {
    pub fn is_enabled_for(&self, app: &AppType) -> bool {
        match app {
            AppType::Claude => self.claude,
            AppType::Codex => self.codex,
            AppType::Gemini => self.gemini,
            AppType::GrokBuild => self.grokbuild,
            AppType::OpenCode => self.opencode,
            AppType::Hermes => self.hermes,
            AppType::Pi => self.pi,
            AppType::Mcode => self.mcode,
            AppType::Kimi => self.kimi,
            AppType::Dsh => self.dsh,
            AppType::Agy => self.agy,
            AppType::Omp => self.omp,
            AppType::Copilot => self.copilot,
            AppType::WorkBuddy => self.workbuddy,
            AppType::OpenClaw | AppType::ClaudeDesktop => false,
        }
    }

    pub fn set_enabled_for(&mut self, app: &AppType, enabled: bool) {
        match app {
            AppType::Claude => self.claude = enabled,
            AppType::Codex => self.codex = enabled,
            AppType::Gemini => self.gemini = enabled,
            AppType::GrokBuild => self.grokbuild = enabled,
            AppType::OpenCode => self.opencode = enabled,
            AppType::Hermes => self.hermes = enabled,
            AppType::Pi => self.pi = enabled,
            AppType::Mcode => self.mcode = enabled,
            AppType::Kimi => self.kimi = enabled,
            AppType::Dsh => self.dsh = enabled,
            AppType::Agy => self.agy = enabled,
            AppType::Omp => self.omp = enabled,
            AppType::Copilot => self.copilot = enabled,
            AppType::WorkBuddy => self.workbuddy = enabled,
            AppType::OpenClaw | AppType::ClaudeDesktop => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// File name stem. Also the list key in the frontend; it changes with a rename.
    pub id: String,
    pub name: String,
    pub content: String,
    /// List summary: the first body line (skipping frontmatter and headings).
    /// Computed by the backend and not persisted.
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub apps: RuleApps,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// How rules reach an app. The frontend uses this for badges and hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleInjection {
    /// The app reads the directory natively; no extra configuration.
    Native,
    /// The app loads the directory through a glob in its instructions config.
    Instructions,
    /// The app has no rules directory and needs a SessionStart hook.
    Hook,
    /// The directory is synced, but only the user's own plugin/extension reads it.
    Manual,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAppSupport {
    pub app: String,
    pub injection: RuleInjection,
    /// Rule file extension without the leading dot. Currently always `md`.
    pub extension: String,
    pub dir: String,
    pub display_dir: String,
    /// Whether step 3 must edit the app's settings file; false for apps that
    /// read the directory natively.
    pub needs_settings: bool,
}

/// A rule file found by Discover: it lives in some app's rules directory and may
/// not be managed by CC Switch yet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverableRule {
    /// File name stem, which becomes the rule name after import.
    pub name: String,
    /// First body line, used as the list summary.
    pub preview: String,
    /// Apps whose rules directories contain this file.
    pub apps: Vec<String>,
    /// A rule with the same name already exists in the database.
    pub managed: bool,
    /// Id of the matching rule when managed (the id of a renamed rule can differ
    /// from the file name).
    pub rule_id: Option<String>,
    pub path: String,
    pub display_path: String,
}

#[derive(Debug, Clone)]
pub struct RuleSupport {
    pub injection: RuleInjection,
    pub dir: PathBuf,
    pub extension: &'static str,
}

const DEFAULT_EXTENSION: &str = "md";

/// Base config directory for each app. WorkBuddy prompt files live under an
/// account directory, but the CodeBuddy engine reads rules from the data
/// directory, so resolve that stable directory explicitly here.
fn app_base_dir(app: &AppType) -> Option<PathBuf> {
    if matches!(app, AppType::ClaudeDesktop | AppType::OpenClaw) {
        return None;
    }
    if matches!(app, AppType::WorkBuddy) {
        return Some(crate::workbuddy_config::data_dir());
    }
    crate::prompt_files::prompt_file_path(app)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

pub fn rule_support(app: &AppType) -> Option<RuleSupport> {
    if !SUPPORTED_APPS.contains(app) {
        return None;
    }
    let (injection, dir) = match app {
        AppType::Claude => (RuleInjection::Native, app_base_dir(app)?.join("rules")),
        AppType::Codex => {
            // Codex relies on a SessionStart hook, and Windows has no usable
            // command wrapper.
            if cfg!(windows) {
                return None;
            }
            (RuleInjection::Hook, app_base_dir(app)?.join("rules"))
        }
        // Gemini CLI has no rules directory; a SessionStart hook injects rules
        // into context.
        AppType::Gemini => (RuleInjection::Hook, app_base_dir(app)?.join("rules")),
        // OpenCode reads a glob from the global `instructions` config; the
        // directory itself only stores files.
        AppType::OpenCode => (
            RuleInjection::Instructions,
            app_base_dir(app)?.join("rules"),
        ),
        _ => return None,
    };
    Some(RuleSupport {
        injection,
        dir,
        extension: DEFAULT_EXTENSION,
    })
}

impl RuleSupport {
    /// Whether step 3 must edit the app's settings file. Native reading and
    /// user-plugin injection do not need it.
    pub fn needs_settings(&self) -> bool {
        matches!(
            self.injection,
            RuleInjection::Instructions | RuleInjection::Hook
        )
    }

    /// A rule id is used directly as a file name; callers must verify it is a
    /// safe single path segment first.
    pub fn file_path(&self, rule_id: &str) -> Option<PathBuf> {
        is_safe_rule_id(rule_id).then(|| self.dir.join(format!("{rule_id}.{}", self.extension)))
    }
}

/// The manifest is user-writable, so ids in it are untrusted input: accept only
/// single path segments and reject path separators, drive prefixes, and names
/// starting with a dot, so `../` cannot escape the rules directory.
pub fn is_safe_rule_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
                )
        })
}

/// The three rule sync steps. The template method always runs them in this
/// order, and any failure carries the step it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSyncStep {
    /// Step 1: resolve the install location.
    Locate,
    /// Step 2: copy or generate the rule files.
    WriteFiles,
    /// Step 3: edit settings so the app reads the rules; a no-op for apps that
    /// read them natively.
    Enable,
}

impl RuleSyncStep {
    pub fn number(self) -> u8 {
        match self {
            Self::Locate => 1,
            Self::WriteFiles => 2,
            Self::Enable => 3,
        }
    }

    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::Locate => ("确认安装位置", "locate install directory"),
            Self::WriteFiles => ("写入规则文件", "write rule files"),
            Self::Enable => ("启用应用读取", "enable app integration"),
        }
    }
}

/// How one app installs rules: locate the directory, render files, enable
/// reading.
///
/// The service layer calls these three steps in a fixed order when creating or
/// syncing rules, without branching on native vs. hook; step 3 is a no-op for
/// natively supported apps. Supporting a new app only requires implementing this
/// trait.
pub trait RuleAdapter: Sync {
    fn app(&self) -> AppType;

    /// Step 1: resolve the rules install location, extension, and injection
    /// mode. The directory always comes from `rule_support`.
    fn locate(&self) -> Result<RuleSupport, AppError> {
        require_support(&self.app())
    }

    /// Step 2: render one rule into file content the app can read.
    fn render(&self, rule: &Rule) -> String;

    /// Step 3: edit app settings so it loads the rules directory. A no-op when
    /// there are no settings to change.
    fn apply_integration(&self, db: &Database, enabled: bool) -> Result<(), AppError>;
}

/// Every wired-up app shares the same adapter: rendering goes through
/// `render_content`, and only the step 3 function differs. Adding an app means
/// adding one static instance.
struct AppRules {
    app: AppType,
    integrate: fn(&AppType, &Database, bool) -> Result<(), AppError>,
}

impl RuleAdapter for AppRules {
    fn app(&self) -> AppType {
        self.app.clone()
    }

    fn render(&self, rule: &Rule) -> String {
        render_content(&self.app, rule)
    }

    fn apply_integration(&self, db: &Database, enabled: bool) -> Result<(), AppError> {
        (self.integrate)(&self.app, db, enabled)
    }
}

/// Reads the rules directory natively: step 3 is a no-op.
fn no_integration(_app: &AppType, _db: &Database, _enabled: bool) -> Result<(), AppError> {
    Ok(())
}

/// Install or remove the SessionStart hook (Codex, Gemini CLI).
fn hook_integration(app: &AppType, db: &Database, enabled: bool) -> Result<(), AppError> {
    crate::rules_hook::set_hook_installed(db, app, enabled)
}

/// Add or remove the rules directory glob in OpenCode's global `instructions`.
fn opencode_integration(_app: &AppType, _db: &Database, enabled: bool) -> Result<(), AppError> {
    crate::opencode_config::set_rules_instructions(enabled)
}

static CLAUDE_RULES: AppRules = AppRules {
    app: AppType::Claude,
    integrate: no_integration,
};
static CODEX_RULES: AppRules = AppRules {
    app: AppType::Codex,
    integrate: hook_integration,
};
static GEMINI_RULES: AppRules = AppRules {
    app: AppType::Gemini,
    integrate: hook_integration,
};
static OPENCODE_RULES: AppRules = AppRules {
    app: AppType::OpenCode,
    integrate: opencode_integration,
};

pub fn rule_adapter(app: &AppType) -> Option<&'static dyn RuleAdapter> {
    match app {
        AppType::Claude => Some(&CLAUDE_RULES),
        AppType::Codex => Some(&CODEX_RULES),
        AppType::Gemini => Some(&GEMINI_RULES),
        AppType::OpenCode => Some(&OPENCODE_RULES),
        _ => None,
    }
}

fn require_support(app: &AppType) -> Result<RuleSupport, AppError> {
    rule_support(app).ok_or_else(|| unsupported_app(app))
}

pub fn app_supports(app: &AppType) -> bool {
    rule_support(app).is_some()
}

pub fn supported_apps() -> Vec<AppType> {
    SUPPORTED_APPS
        .into_iter()
        .filter(|app| rule_support(app).is_some())
        .collect()
}

/// List candidate rule files in a directory (only plain file names with the
/// given extension; hidden files are skipped). Discover and hook injection share
/// this rule so that what is visible matches what is injected.
pub fn list_rule_files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, AppError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io(dir, error)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io(dir, error))?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') || !path.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub fn app_support_list() -> Vec<RuleAppSupport> {
    supported_apps()
        .into_iter()
        .filter_map(|app| {
            let support = rule_support(&app)?;
            Some(RuleAppSupport {
                app: app.as_str().to_string(),
                injection: support.injection,
                extension: support.extension.to_string(),
                display_dir: crate::prompt_files::display_path(&support.dir),
                needs_settings: support.needs_settings(),
                dir: support.dir.to_string_lossy().to_string(),
            })
        })
        .collect()
}

pub fn unsupported_app(app: &AppType) -> AppError {
    AppError::localized(
        "rules.unsupported_app",
        format!("{} 不支持 Rules", app.as_str()),
        format!("Rules are not supported for {}", app.as_str()),
    )
}

/// A rule name is also its file name (`<name>.md`): readable characters such as
/// CJK text and spaces are allowed, but path separators, control characters, and
/// characters Windows forbids are not, nor may the name start or end with `.`.
pub fn validate_rule_name(name: &str) -> Result<(), AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("Rule name is required".into()));
    }
    if trimmed.chars().count() > 80 {
        return Err(AppError::InvalidInput(
            "Rule name must be 1-80 characters".into(),
        ));
    }
    if trimmed.starts_with('.') {
        return Err(AppError::InvalidInput(
            "Rule name must not start with '.'".into(),
        ));
    }
    if trimmed.ends_with('.') {
        return Err(AppError::InvalidInput(
            "Rule name must not end with '.'".into(),
        ));
    }
    const FORBIDDEN: [char; 9] = ['/', '\\', '<', '>', ':', '"', '|', '?', '*'];
    if trimmed
        .chars()
        .any(|character| character.is_control() || FORBIDDEN.contains(&character))
    {
        return Err(AppError::InvalidInput(
            "Rule name may not contain control characters or / \\ < > : \" | ? *".into(),
        ));
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if is_windows_reserved(&stem) {
        return Err(AppError::InvalidInput(
            "Rule name is reserved by Windows".into(),
        ));
    }
    // File names are capped at 255 bytes; leave room for `.md` while still
    // fitting 80 CJK characters.
    if trimmed.len() > 240 {
        return Err(AppError::InvalidInput("Rule name is too long".into()));
    }
    Ok(())
}

fn is_windows_reserved(stem: &str) -> bool {
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && (b'1'..=b'9').contains(&bytes[3])
}

/// Legacy versions derived a slug from the name as a stable id; those ids must
/// remain readable and updatable.
pub fn validate_rule_id(id: &str) -> Result<(), AppError> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 80 {
        return Err(AppError::InvalidInput(
            "Rule id must be 1-80 characters".into(),
        ));
    }
    if trimmed.starts_with('.') {
        return Err(AppError::InvalidInput(
            "Rule id must not start with '.'".into(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(AppError::InvalidInput(
            "Rule id may only contain letters, digits, '.', '_' and '-'".into(),
        ));
    }
    Ok(())
}

pub fn validate_rule_content(content: &str) -> Result<(), AppError> {
    if content.len() > MAX_RULE_CONTENT_BYTES {
        return Err(AppError::InvalidInput(format!(
            "Rule content must not exceed {} KiB",
            MAX_RULE_CONTENT_BYTES / 1024
        )));
    }
    Ok(())
}

pub fn validate_rule(rule: &Rule) -> Result<(), AppError> {
    validate_rule_name(&rule.name)?;
    if rule.id != rule.name {
        // Legacy rules keep their slug id and are validated with the old rules.
        validate_rule_id(&rule.id)?;
    }
    validate_rule_content(&rule.content)?;
    Ok(())
}

/// Name comparison is case-insensitive (Unicode, matching macOS / Windows file
/// systems): `Style` and `style` collide on one file, so they count as the same
/// name from the start.
pub fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Split YAML frontmatter. Returns `(Some(frontmatter_without_delimiters), body)`.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = normalized.strip_prefix("---") else {
        return (None, content);
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    for marker in ["\n---", "\r\n---"] {
        if let Some(index) = rest.find(marker) {
            let after = &rest[index + marker.len()..];
            let body = after
                .strip_prefix("\r\n")
                .or_else(|| after.strip_prefix('\n'))
                .unwrap_or(after);
            let frontmatter = rest[..index].trim_end();
            return (Some(frontmatter), body);
        }
    }
    (None, content)
}

pub fn strip_frontmatter(content: &str) -> &str {
    split_frontmatter(content).1
}

/// First line of the rule body for the list summary. Skips frontmatter and empty
/// lines and strips leading `#` characters.
pub fn content_preview(content: &str) -> String {
    for line in strip_frontmatter(content).lines() {
        let line = line.trim().trim_start_matches('#').trim();
        if line.is_empty() {
            continue;
        }
        let mut characters = line.chars();
        let preview: String = characters.by_ref().take(120).collect();
        return if characters.next().is_some() {
            format!("{preview}…")
        } else {
            preview
        };
    }
    String::new()
}

/// Copilot's path-scope field is `applyTo`; map a Claude-style `paths:` list to a
/// comma-separated glob string. When `applyTo` already exists, keep it and only
/// drop `paths`.
fn render_copilot(content: &str) -> String {
    let (frontmatter, body) = split_frontmatter(content);
    let Some(frontmatter) = frontmatter else {
        return content.to_string();
    };

    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut output: Vec<String> = Vec::new();
    let mut has_apply_to = false;
    let mut paths: Option<String> = None;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if let Some(value) = trimmed.strip_prefix("paths:") {
            let mut globs = Vec::new();
            push_globs(value, &mut globs);
            index += 1;
            while index < lines.len() {
                let nested = lines[index].trim_start();
                if nested.starts_with("- ") || nested == "-" {
                    push_globs(&nested[1..], &mut globs);
                    index += 1;
                } else if nested.is_empty() {
                    index += 1;
                } else {
                    break;
                }
            }
            paths = Some(globs.join(","));
            continue;
        }
        if trimmed.starts_with("applyTo:") {
            has_apply_to = true;
        }
        output.push(line.to_string());
        index += 1;
    }

    if let Some(globs) = paths {
        if !has_apply_to && !globs.is_empty() {
            output.push(format!("applyTo: \"{globs}\""));
        }
    }

    if output.is_empty() {
        return body.to_string();
    }
    format!("---\n{}\n---\n\n{}", output.join("\n"), body)
}

fn push_globs(raw: &str, globs: &mut Vec<String>) {
    for part in raw.split(',') {
        let value = part.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            globs.push(value.to_string());
        }
    }
}

/// Render a rule into file content the target app understands (step 2).
///
/// Apps that do not understand YAML frontmatter (hook / instructions / manual)
/// get it stripped, so things like `paths:` are not injected verbatim into the
/// prompt. Branches for unsupported apps are kept; wiring one up only requires
/// adding it to `rule_adapter`.
fn render_content(app: &AppType, rule: &Rule) -> String {
    let content = rule.content.as_str();
    match app {
        AppType::Copilot => render_copilot(content),
        AppType::Claude | AppType::GrokBuild | AppType::WorkBuddy | AppType::Agy => {
            content.to_string()
        }
        AppType::Codex
        | AppType::Gemini
        | AppType::Kimi
        | AppType::OpenCode
        | AppType::Hermes
        | AppType::Pi
        | AppType::Mcode
        | AppType::Dsh
        | AppType::Omp => strip_frontmatter(content).to_string(),
        // `render_rule_for_app` / `rule_adapter` reject these two apps first.
        AppType::OpenClaw | AppType::ClaudeDesktop => content.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(content: &str) -> Rule {
        Rule {
            id: "style".into(),
            name: "Style".into(),
            content: content.into(),
            preview: content_preview(content),
            apps: RuleApps::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn validates_legacy_rule_ids() {
        assert!(validate_rule_id("style-guide").is_ok());
        assert!(validate_rule_id("style_guide.v2").is_ok());
        assert!(validate_rule_id("").is_err());
        assert!(validate_rule_id("../escape").is_err());
        assert!(validate_rule_id(".hidden").is_err());
        assert!(validate_rule_id("中文").is_err());
    }

    #[test]
    fn validates_rule_names() {
        assert!(validate_rule_name("Style Guide").is_ok());
        assert!(validate_rule_name("中文规则").is_ok());
        assert!(validate_rule_name("").is_err());
        assert!(validate_rule_name("../escape").is_err());
        assert!(validate_rule_name(".hidden").is_err());
        assert!(validate_rule_name("a/b").is_err());
        assert!(validate_rule_name("ends.").is_err());
        assert!(validate_rule_name("CON").is_err());
        assert!(validate_rule_name("com1").is_err());
    }

    #[test]
    fn preview_skips_frontmatter_and_headings() {
        assert_eq!(
            content_preview("---\npaths: src/**\n---\n\n# Style\n\nBody"),
            "Style"
        );
        assert_eq!(content_preview(""), "");
    }

    #[test]
    fn copilot_maps_paths_to_apply_to() {
        let rendered = render_copilot("---\npaths:\n  - src/**\n---\nbody");
        assert!(rendered.contains("applyTo:"));
        assert!(!rendered.contains("paths:"));
        assert!(rendered.contains("src/**"));
    }

    #[test]
    fn hook_apps_drop_frontmatter() {
        let rendered = rule_adapter(&AppType::Codex)
            .unwrap()
            .render(&rule("---\npaths: src/**\n---\nbody"));
        assert_eq!(rendered, "body");
    }

    #[test]
    fn claude_keeps_frontmatter() {
        let content = "---\npaths: src/**\n---\nbody";
        let rendered = rule_adapter(&AppType::Claude)
            .unwrap()
            .render(&rule(content));
        assert_eq!(rendered, content);
    }

    #[test]
    fn rule_file_names_use_app_extension() {
        let claude = require_support(&AppType::Claude).unwrap();
        let path = claude.file_path("style").unwrap();
        assert_eq!(path.file_name().unwrap(), "style.md");
        // The name is the file name; CJK characters and spaces are preserved.
        let path = claude.file_path("TypeScript 风格").unwrap();
        assert_eq!(path.file_name().unwrap(), "TypeScript 风格.md");
    }

    #[test]
    fn adapters_run_the_same_three_steps() {
        assert_eq!(
            [
                RuleSyncStep::Locate,
                RuleSyncStep::WriteFiles,
                RuleSyncStep::Enable
            ]
            .map(RuleSyncStep::number),
            [1, 2, 3]
        );

        let claude = rule_adapter(&AppType::Claude).unwrap();
        let plan = claude.locate().unwrap();
        assert_eq!(plan.dir.file_name().unwrap(), "rules");
        assert!(!plan.needs_settings());

        let codex = rule_adapter(&AppType::Codex).unwrap();
        if cfg!(windows) {
            // Windows has no usable hook command wrapper, so step 1 rejects it.
            assert!(codex.locate().is_err());
        } else {
            assert!(codex.locate().unwrap().needs_settings());
        }
        assert_eq!(codex.render(&rule("body")), "body");

        // Gemini uses a hook: it needs settings.json edits and frontmatter is
        // stripped from the body.
        let gemini = rule_adapter(&AppType::Gemini).unwrap();
        assert!(gemini.locate().unwrap().needs_settings());
        assert_eq!(
            gemini.render(&rule("---\npaths: src/**\n---\nbody")),
            "body"
        );

        // OpenCode references the directory from config: files are synced, but
        // opencode.json still needs an entry.
        let opencode = rule_adapter(&AppType::OpenCode).unwrap();
        assert!(opencode.locate().unwrap().needs_settings());
        assert_eq!(opencode.render(&rule("body")), "body");
    }

    #[test]
    fn supported_apps_cover_the_four_adapters() {
        let expected = if cfg!(windows) {
            vec![AppType::Claude, AppType::Gemini, AppType::OpenCode]
        } else {
            vec![
                AppType::Claude,
                AppType::Codex,
                AppType::Gemini,
                AppType::OpenCode,
            ]
        };
        assert_eq!(supported_apps(), expected);
        assert!(rule_support(&AppType::ClaudeDesktop).is_none());
        assert!(rule_support(&AppType::Copilot).is_none());
        // The render branch exists, but unsupported apps must be blocked by the
        // adapter matrix.
        assert!(rule_adapter(&AppType::Copilot).is_none());
        // Antigravity and DSH are not enabled yet.
        assert!(rule_support(&AppType::Agy).is_none());
        assert!(rule_adapter(&AppType::Agy).is_none());
        assert!(rule_support(&AppType::Dsh).is_none());
        assert!(rule_adapter(&AppType::Dsh).is_none());
    }

    #[test]
    fn support_list_reports_each_apps_injection() {
        let list = app_support_list();
        let find = |app: &str| list.iter().find(|item| item.app == app).unwrap();

        assert_eq!(find("claude").injection, RuleInjection::Native);
        assert_eq!(find("gemini").injection, RuleInjection::Hook);
        assert_eq!(find("opencode").injection, RuleInjection::Instructions);
        assert!(find("gemini").needs_settings);
        assert!(find("opencode").needs_settings);
        // Every rules directory is named rules, so step 2 writes directly into it.
        for item in &list {
            assert!(item.dir.ends_with("rules"), "{}: {}", item.app, item.dir);
        }
    }

    #[test]
    fn rejects_oversized_rule_content() {
        assert!(validate_rule_content("body").is_ok());
        let oversized = "a".repeat(MAX_RULE_CONTENT_BYTES + 1);
        assert!(validate_rule_content(&oversized).is_err());
    }

    #[test]
    fn rule_ids_cannot_escape_the_rules_directory() {
        assert!(is_safe_rule_id("TypeScript 风格"));
        assert!(is_safe_rule_id("style_guide.v2"));
        assert!(!is_safe_rule_id("../escape"));
        assert!(!is_safe_rule_id("a/b"));
        assert!(!is_safe_rule_id("C:\\Windows"));
        assert!(!is_safe_rule_id(".hidden"));
        assert!(!is_safe_rule_id(""));
        let claude = require_support(&AppType::Claude).unwrap();
        assert!(claude.file_path("../escape").is_none());
    }

    #[test]
    fn rule_names_compare_case_insensitively() {
        assert!(same_name("Style", "style"));
        assert!(same_name("Правило", "правило"));
        assert!(same_name("中文", "中文"));
        assert!(!same_name("style", "styles"));
    }
}
