//! Inject rules into apps that have no native rules directory.
//!
//! Injection uses a SessionStart hook: when the app starts a session it calls
//! cc-switch's hidden CLI (`--cc-switch-rules-hook <app>`), which reads the
//! files in that app's rules directory and prints them to stdout. Output formats
//! differ per app:
//! - Codex: plain-text stdout becomes extra developer context;
//! - Gemini CLI: must emit strict JSON (`hookSpecificOutput.additionalContext`);
//! - Kimi CLI: plain-text stdout is added to context.
//!
//! This module also installs and removes the hook configuration. Every edit
//! preserves the user's other hooks.
//!
//! Codex and Gemini CLI are wired into the rules adapter; Kimi's installer and
//! CLI output format are implemented and can be reused once its adapter is
//! enabled.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::live::engine::LiveFile;
use crate::live::patch::json::JsonPatch;
use crate::live::patch::toml::TomlPatch;
use crate::live::patch::KeyPath;
use crate::mode::operation::{AppWrite, FileChange};
use crate::mode::state::{op, PendingTarget};

pub const HOOK_MARKER: &str = "--cc-switch-rules-hook";

const SESSION_START_EVENT: &str = "SessionStart";
const GEMINI_HOOK_NAME: &str = "cc-switch-rules";

/// Hidden CLI: print the given app's rules to stdout. Returns the process exit
/// code.
pub fn run_cli(app: &str) -> i32 {
    match run_cli_inner(app) {
        Ok(()) => 0,
        Err(error) => {
            // A SessionStart hook must not block the user from starting a session.
            eprintln!("cc-switch rules hook: {error}");
            0
        }
    }
}

fn run_cli_inner(app: &str) -> Result<(), AppError> {
    let app_type = app.parse::<AppType>()?;
    let context = rules_context(&app_type)?;

    match app_type {
        AppType::Gemini => {
            let value = if context.is_empty() {
                json!({})
            } else {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": context,
                    }
                })
            };
            println!(
                "{}",
                serde_json::to_string(&value).map_err(|e| AppError::Config(format!(
                    "Failed to serialize hook output: {e}"
                )))?
            );
        }
        AppType::Codex | AppType::Kimi => {
            if !context.is_empty() {
                print!("{context}");
            }
        }
        other => {
            return Err(AppError::localized(
                "rules.hook_unsupported",
                format!("{} 不需要 hook 注入", other.as_str()),
                format!("{} does not use hook injection", other.as_str()),
            ));
        }
    }
    Ok(())
}

/// Read every rule file in an app's rules directory and join them into one
/// context block.
pub fn rules_context(app: &AppType) -> Result<String, AppError> {
    let support =
        crate::rules::rule_support(app).ok_or_else(|| crate::rules::unsupported_app(app))?;
    let files = crate::rules::list_rule_files(&support.dir, support.extension)?;
    Ok(read_rule_sections(&files).join("\n\n---\n\n"))
}

/// Read the non-empty bodies of the given rule files. An unreadable file (bad
/// permissions, non-UTF-8, deleted between scan and read) is skipped with a
/// warning so one bad file cannot drop every other rule from the context.
fn read_rule_sections(files: &[PathBuf]) -> Vec<String> {
    let mut sections = Vec::new();
    for path in files {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let content = content.trim();
                if !content.is_empty() {
                    sections.push(content.to_string());
                }
            }
            Err(error) => log::warn!("Skipping unreadable rule file {}: {error}", path.display()),
        }
    }
    sections
}

/// Install or remove the SessionStart hook for a `Hook`-mode app.
pub fn set_hook_installed(db: &Database, app: &AppType, installed: bool) -> Result<(), AppError> {
    match app {
        AppType::Codex => set_codex_hook(db, installed),
        AppType::Gemini => set_gemini_hook(db, installed),
        AppType::Kimi => set_kimi_hook(installed),
        _ => Ok(()),
    }
}

fn hook_command(app: &AppType) -> Result<String, AppError> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Config(format!("Cannot locate cc-switch executable: {e}")))?;
    Ok(format!(
        "{} {HOOK_MARKER} {}",
        shell_quote(&exe.to_string_lossy()),
        app.as_str()
    ))
}

#[cfg(target_os = "windows")]
fn shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn contains_marker(value: &Value) -> bool {
    serde_json::to_string(value)
        .map(|text| text.contains(HOOK_MARKER))
        .unwrap_or(false)
}

fn read_json_object(path: &Path) -> Result<serde_json::Map<String, Value>, AppError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                return Ok(serde_json::Map::new());
            }
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(map)) => Ok(map),
                Ok(_) => Err(AppError::Config(format!(
                    "{} must contain a JSON object",
                    path.display()
                ))),
                Err(error) => Err(AppError::json(path, error)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(error) => Err(AppError::io(path, error)),
    }
}

/// Compute the desired `hooks.<event>` content and return the patch to write;
/// `None` when nothing changed. `entry` of `None` means remove the cc-switch
/// entry.
fn json_hook_patch(
    root: &serde_json::Map<String, Value>,
    event: &str,
    entry: Option<Value>,
) -> Result<Option<JsonPatch>, AppError> {
    let empty = serde_json::Map::new();
    let hooks = match root.get("hooks") {
        None => &empty,
        Some(Value::Object(hooks)) => hooks,
        Some(_) => {
            return Err(AppError::Config(
                "Existing \"hooks\" value must be a JSON object".into(),
            ))
        }
    };
    let groups: Vec<Value> = match hooks.get(event) {
        None => Vec::new(),
        Some(Value::Array(groups)) => groups.clone(),
        Some(_) => {
            return Err(AppError::Config(format!(
                "Existing hooks.{event} must be an array"
            )))
        }
    };

    // Remove the old cc-switch entry before appending: the executable path in
    // the command can change when the app is upgraded.
    let mut merged = groups
        .iter()
        .filter(|group| !contains_marker(group))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(entry) = entry {
        merged.push(entry);
    }
    if merged == groups {
        return Ok(None);
    }

    if merged.is_empty() {
        // If removing leaves the hooks object empty, drop the wrapper too instead
        // of leaving an empty object behind.
        let remove = if hooks.keys().all(|key| key == event) {
            vec![KeyPath::new(&["hooks"])]
        } else {
            vec![KeyPath::new(&["hooks", event])]
        };
        return Ok(Some(JsonPatch {
            remove,
            ..JsonPatch::default()
        }));
    }
    Ok(Some(JsonPatch {
        set: vec![(KeyPath::new(&["hooks", event]), Value::Array(merged))],
        ..JsonPatch::default()
    }))
}

fn set_codex_hook(db: &Database, installed: bool) -> Result<(), AppError> {
    let path = crate::codex_config::get_codex_config_dir().join("hooks.json");
    let root = read_json_object(&path)?;
    let entry = if installed {
        Some(json!({
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": hook_command(&AppType::Codex)?,
            }],
        }))
    } else {
        None
    };
    if let Some(patch) = json_hook_patch(&root, SESSION_START_EVENT, entry)? {
        // hooks.json also goes through the write engine: first-write backup,
        // per-app locking, and crash recovery.
        AppWrite::begin(db, AppType::Codex.as_str())?.run(
            op::APPLY,
            &[FileChange {
                file: LiveFile::shared(&path),
                patch: &patch,
            }],
            PendingTarget::default(),
        )?;
    }
    if installed {
        enable_codex_hooks_feature(db)?;
    }
    Ok(())
}

/// Codex hooks are off by default and need `[features] codex_hooks = true`.
/// Removing rules does not turn the flag back off, since the user may have
/// enabled it themselves.
fn enable_codex_hooks_feature(db: &Database) -> Result<(), AppError> {
    let text = crate::codex_config::read_codex_config_text()?;
    if crate::codex_config::codex_config_feature_enabled(&text, "codex_hooks") {
        return Ok(());
    }
    let patch = TomlPatch {
        set: vec![(
            KeyPath::new(&["features", "codex_hooks"]),
            toml_edit::value(true),
        )],
        ..TomlPatch::default()
    };
    AppWrite::begin(db, AppType::Codex.as_str())?.run(
        op::APPLY,
        &[FileChange {
            file: LiveFile::private(crate::codex_config::get_codex_config_path()),
            patch: &patch,
        }],
        PendingTarget::default(),
    )?;
    Ok(())
}

fn set_gemini_hook(db: &Database, installed: bool) -> Result<(), AppError> {
    let path = crate::gemini_config::get_gemini_settings_path();
    let root = read_json_object(&path)?;
    let entry = if installed {
        Some(json!({
            "hooks": [{
                "type": "command",
                "name": GEMINI_HOOK_NAME,
                "command": hook_command(&AppType::Gemini)?,
                "timeout": 30000,
            }],
        }))
    } else {
        None
    };
    if let Some(patch) = json_hook_patch(&root, SESSION_START_EVENT, entry)? {
        // Like Codex, go through the write engine: settings.json holds other
        // settings that must not be clobbered.
        AppWrite::begin(db, AppType::Gemini.as_str())?.run(
            op::APPLY,
            &[FileChange {
                file: LiveFile::shared(&path),
                patch: &patch,
            }],
            PendingTarget::default(),
        )?;
    }
    Ok(())
}

fn set_kimi_hook(installed: bool) -> Result<(), AppError> {
    let path = crate::kimi_config::config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::io(&path, error)),
    };
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| AppError::Config(format!("Invalid Kimi config.toml: {error}")))?;

    match document.get_mut("hooks") {
        Some(Item::ArrayOfTables(_)) => {}
        Some(_) => {
            return Err(AppError::Config(
                "Kimi config.toml [hooks] must be an array of tables".into(),
            ));
        }
        None => {
            if !installed {
                return Ok(());
            }
            document["hooks"] = Item::ArrayOfTables(ArrayOfTables::new());
        }
    }

    let before = document.to_string();
    let hooks = document["hooks"]
        .as_array_of_tables_mut()
        .ok_or_else(|| AppError::Config("Kimi hooks must be an array of tables".into()))?;

    let mut kept = ArrayOfTables::new();
    for table in hooks.iter() {
        if !table.to_string().contains(HOOK_MARKER) {
            kept.push(table.clone());
        }
    }

    if installed {
        let mut table = Table::new();
        table["event"] = toml_edit::value(SESSION_START_EVENT);
        table["matcher"] = toml_edit::value("startup|resume");
        table["command"] = toml_edit::value(hook_command(&AppType::Kimi)?);
        table["timeout"] = toml_edit::value(30_i64);
        kept.push(table);
    }

    if kept.is_empty() {
        document.remove("hooks");
    } else {
        document["hooks"] = Item::ArrayOfTables(kept);
    }
    // Compare after re-serializing: a changed command path (app upgrade) must
    // also trigger a refresh, not just the presence of the marker.
    let after = document.to_string();
    if after == before {
        return Ok(());
    }
    crate::config::write_text_file_private(&path, &after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_cc_switch_entries() {
        use crate::live::patch::LivePatch;

        let root: serde_json::Map<String, Value> = serde_json::from_str(
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "user-hook"}]},
                        {"hooks": [{"type": "command", "command": "'/bin/cc-switch' --cc-switch-rules-hook codex"}]}
                    ]
                }
            }"#,
        )
        .unwrap();

        let patch = json_hook_patch(&root, SESSION_START_EVENT, None)
            .unwrap()
            .expect("cc-switch entry must be removed");
        let pre = serde_json::to_vec(&Value::Object(root)).unwrap();
        let out = patch.apply(Path::new("settings.json"), Some(&pre)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("user-hook"));
        assert!(!text.contains(HOOK_MARKER));
    }

    #[test]
    fn does_not_add_duplicate_entries() {
        use crate::live::patch::LivePatch;

        let entry = json!({
            "hooks": [{"type": "command", "command": "'/bin/cc-switch' --cc-switch-rules-hook codex"}]
        });
        let root = serde_json::Map::new();
        let patch = json_hook_patch(&root, SESSION_START_EVENT, Some(entry.clone()))
            .unwrap()
            .expect("first install must write");
        let out = patch.apply(Path::new("hooks.json"), None).unwrap();
        let root: serde_json::Map<String, Value> = serde_json::from_slice(&out).unwrap();
        assert!(json_hook_patch(&root, SESSION_START_EVENT, Some(entry))
            .unwrap()
            .is_none());
        assert_eq!(
            root["hooks"][SESSION_START_EVENT].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn refreshes_stale_cc_switch_command_without_touching_user_hooks() {
        use crate::live::patch::LivePatch;

        let root: serde_json::Map<String, Value> = serde_json::from_str(
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "user-hook"}]},
                        {"hooks": [{"type": "command", "command": "'/old/cc-switch' --cc-switch-rules-hook codex"}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        let entry = json!({
            "hooks": [{"type": "command", "command": "'/new/cc-switch' --cc-switch-rules-hook codex"}]
        });
        let patch = json_hook_patch(&root, "SessionStart", Some(entry.clone()))
            .unwrap()
            .expect("stale command must be refreshed");
        let pre = serde_json::to_vec(&Value::Object(root)).unwrap();
        let out = patch.apply(Path::new("hooks.json"), Some(&pre)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("user-hook"));
        assert!(text.contains("/new/cc-switch"));
        assert!(!text.contains("/old/cc-switch"));

        // The same content computed again must not hit the disk a second time.
        let root: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
        assert!(json_hook_patch(&root, "SessionStart", Some(entry))
            .unwrap()
            .is_none());
    }

    #[test]
    fn unreadable_rule_files_are_skipped_without_dropping_others() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.md");
        let blank = dir.path().join("blank.md");
        let bad = dir.path().join("bad.md");
        let missing = dir.path().join("missing.md");
        std::fs::write(&first, "  first rule\n\n").unwrap();
        std::fs::write(&blank, " \n\t\n").unwrap();
        std::fs::write(&bad, [0xff, 0xfe, 0xfd]).unwrap();

        let sections = read_rule_sections(&[first, blank, bad, missing]);
        assert_eq!(sections, vec!["first rule".to_string()]);
    }
}
