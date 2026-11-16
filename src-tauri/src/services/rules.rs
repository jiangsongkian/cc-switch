//! Rules service layer: CRUD, per-app toggles, file projection, and hook
//! installation.
//!
//! The database is the single source of truth; each app's rules directory is
//! only a projection. Every projected directory contains a
//! `.cc-switch-rules.json` manifest that distinguishes files written by CC
//! Switch from the user's own rule files. Disabling a rule deletes only the old
//! files listed in the manifest and never touches user files.
//!
//! Mutations project to disk first and commit to the database after success: if
//! sync fails, the command returns an error and the database keeps its previous
//! state. In the worst case disk was written halfway, and the next startup sync
//! repairs it from the database.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::rules::{
    app_supports, content_preview, list_rule_files, rule_adapter, same_name, supported_apps,
    unsupported_app, validate_rule, validate_rule_content, validate_rule_name, DiscoverableRule,
    Rule, RuleApps, RuleSyncStep,
};
use crate::store::AppState;

const MANIFEST_FILE: &str = ".cc-switch-rules.json";

fn rule_state_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_state() -> MutexGuard<'static, ()> {
    // Rule operations only touch the database and file projections; continuing
    // after a panic is safer than disabling the whole feature permanently.
    rule_state_lock()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RuleManifest {
    #[serde(default)]
    rules: Vec<String>,
}

/// Import result. Rules may already be in the database while carrying a sync
/// warning (file projection failed).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub rules: Vec<Rule>,
    pub warning: Option<String>,
}

pub struct RuleService;

impl RuleService {
    pub fn list(state: &AppState) -> Result<Vec<Rule>, AppError> {
        all_rules(state)
    }

    pub fn get(state: &AppState, id: &str) -> Result<Rule, AppError> {
        state.db.get_rule(id)?.ok_or_else(|| not_found(id))
    }

    pub fn create(state: &AppState, name: String, content: String) -> Result<Rule, AppError> {
        let _guard = lock_state();
        let name = name.trim().to_string();
        validate_rule_name(&name)?;
        validate_rule_content(&content)?;

        let existing = state.db.get_all_rules()?;
        if name_taken(&existing, &name, None) {
            return Err(name_taken_error(&name));
        }

        let now = now_millis();
        let rule = Rule {
            // The name doubles as id and file name: whatever the user types is
            // what lands on disk.
            id: name.clone(),
            name,
            preview: content_preview(&content),
            content,
            apps: RuleApps::default(),
            created_at: now,
            updated_at: now,
        };
        validate_rule(&rule)?;
        // A new rule is disabled everywhere by default, so there is no
        // projection to write.
        state.db.save_rule(&rule)?;
        Ok(rule)
    }

    pub fn update(
        state: &AppState,
        id: &str,
        name: String,
        content: String,
    ) -> Result<Rule, AppError> {
        let _guard = lock_state();
        let name = name.trim().to_string();
        validate_rule_name(&name)?;
        validate_rule_content(&content)?;

        let existing = state.db.get_all_rules()?;
        let mut rule = existing.get(id).cloned().ok_or_else(|| not_found(id))?;
        if name_taken(&existing, &name, Some(id)) {
            return Err(name_taken_error(&name));
        }

        // Renaming renames the file: project under the new name first, then
        // replace the old row in a single transaction. Sync removes the old
        // files from each app directory using the manifest.
        let renamed = rule.id != name;
        if renamed {
            rule.id = name.clone();
        }
        rule.name = name;
        rule.preview = content_preview(&content);
        rule.content = content;
        rule.updated_at = now_millis();
        validate_rule(&rule)?;

        let mut all = existing.into_values().collect::<Vec<_>>();
        all.retain(|item| item.id != id);
        all.push(rule.clone());
        sync_all_apps(&all, &state.db)?;

        if renamed {
            state.db.rename_rule(id, &rule)?;
        } else {
            state.db.save_rule(&rule)?;
        }
        Ok(rule)
    }

    pub fn delete(state: &AppState, id: &str) -> Result<(), AppError> {
        let _guard = lock_state();
        let existing = state.db.get_all_rules()?;
        if !existing.contains_key(id) {
            return Err(not_found(id));
        }

        // Remove the rule from projections before deleting the database row, so
        // a failure midway leaves the database in its old state.
        let mut all = existing.into_values().collect::<Vec<_>>();
        all.retain(|rule| rule.id != id);
        sync_all_apps(&all, &state.db)?;

        if !state.db.delete_rule(id)? {
            return Err(not_found(id));
        }
        Ok(())
    }

    pub fn toggle_app(
        state: &AppState,
        id: &str,
        app: &AppType,
        enabled: bool,
    ) -> Result<Rule, AppError> {
        let _guard = lock_state();
        if !app_supports(app) {
            return Err(unsupported_app(app));
        }
        let mut rule = Self::get(state, id)?;
        rule.apps.set_enabled_for(app, enabled);
        rule.updated_at = now_millis();

        let mut all = all_rules(state)?;
        match all.iter_mut().find(|item| item.id == rule.id) {
            Some(slot) => *slot = rule.clone(),
            None => return Err(not_found(id)),
        }
        // Persist only after projection succeeds: if sync fails the command
        // errors and the database keeps the previous toggle state.
        sync_app(app, &all, &state.db)?;
        state.db.save_rule(&rule)?;
        Ok(rule)
    }

    /// Reproject every enabled rule into each app. Called once at startup to
    /// repair inconsistencies from external edits or a previous crash.
    pub fn sync_all(state: &AppState) -> Result<(), AppError> {
        let _guard = lock_state();
        sync_all_apps(&all_rules(state)?, &state.db)
    }

    /// Scan existing `.md` files in each app's rules directory and mark which
    /// ones are already managed by CC Switch.
    pub fn discover(state: &AppState) -> Result<Vec<DiscoverableRule>, AppError> {
        let rules = state.db.get_all_rules()?;
        Ok(scan_rule_files()?
            .into_iter()
            .map(|file| {
                let managed = rules.values().find(|rule| {
                    same_name(&rule.id, &file.name) || same_name(&rule.name, &file.name)
                });
                DiscoverableRule {
                    preview: content_preview(&file.content),
                    apps: file
                        .apps
                        .iter()
                        .map(|app| app.as_str().to_string())
                        .collect(),
                    managed: managed.is_some(),
                    rule_id: managed.map(|rule| rule.id.clone()),
                    display_path: crate::prompt_files::display_path(&file.path),
                    path: file.path.to_string_lossy().to_string(),
                    name: file.name,
                }
            })
            .collect())
    }

    /// Import files selected in Discover into the database. Names that already
    /// exist are skipped so content the user edited in CC Switch is not
    /// overwritten.
    pub fn import(state: &AppState, names: Vec<String>) -> Result<ImportOutcome, AppError> {
        let _guard = lock_state();
        let existing = state.db.get_all_rules()?;
        let scanned = scan_rule_files()?;
        let mut imported_names = HashSet::new();
        let mut created: Vec<Rule> = Vec::new();
        let mut warnings = Vec::new();

        for raw_name in names {
            let name = raw_name.trim().to_string();
            if name.is_empty() || imported_names.contains(&name.to_lowercase()) {
                continue;
            }
            if name_taken(&existing, &name, None) {
                continue;
            }
            let Some(file) = scanned
                .iter()
                .find(|file| same_name(file.name.trim(), &name))
            else {
                continue;
            };
            // Skip unusable names or bodies instead of failing the whole batch:
            // one bad file must not block the other selected files.
            if let Some(reason) = unimportable_reason(&name, &file.content) {
                log::warn!("Skipping rule import {name}: {reason}");
                warnings.push(format!("{name}: {reason}"));
                continue;
            }

            let mut apps = RuleApps::default();
            for app in &file.apps {
                apps.set_enabled_for(app, true);
            }
            let now = now_millis();
            let rule = Rule {
                id: name.clone(),
                name: name.clone(),
                preview: content_preview(&file.content),
                content: file.content.clone(),
                apps,
                created_at: now,
                updated_at: now,
            };
            validate_rule(&rule)?;
            created.push(rule);
            imported_names.insert(name.to_lowercase());
        }

        if created.is_empty() {
            return Ok(ImportOutcome {
                rules: created,
                warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
            });
        }

        // Write the database first (one transaction), then expand file
        // projections from it. With the rules in the database, manifest entries
        // are not treated as stale projections and deleted on the next sync.
        state.db.save_rules(&created)?;

        // The user explicitly asked to import, so adopt existing files into the
        // manifest first; sync will then not treat them as unmanaged same-name
        // files and report a conflict. Apps that fail adoption stay disabled so
        // one app's failure does not roll back the whole import.
        let mut changed = false;
        for rule in &mut created {
            let Some(file) = scanned
                .iter()
                .find(|file| same_name(file.name.trim(), &rule.id))
            else {
                continue;
            };
            for app in &file.apps {
                if !rule.apps.is_enabled_for(app) {
                    continue;
                }
                if let Err(error) = adopt_managed_file(app, &rule.id) {
                    log::warn!(
                        "Failed to adopt rule file {} for {}: {error}",
                        rule.id,
                        app.as_str()
                    );
                    rule.apps.set_enabled_for(app, false);
                    rule.updated_at = now_millis();
                    changed = true;
                    warnings.push(format!("{}: {error}", app.as_str()));
                }
            }
        }
        if changed {
            // This only writes back "enablement failed in some app"; the rule
            // itself is already stored. Downgrade a write failure to a warning:
            // the later sync will surface the conflict again rather than report
            // a successful import as a total failure.
            if let Err(error) = state.db.save_rules(&created) {
                log::warn!("Failed to persist adopted-rule flags: {error}");
                warnings.push(error.to_string());
            }
        }

        if let Err(error) = sync_all_apps(&all_rules(state)?, &state.db) {
            log::warn!("Imported rules but projection failed: {error}");
            warnings.push(error.to_string());
        }

        Ok(ImportOutcome {
            rules: created,
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        })
    }
}

#[derive(Debug)]
struct ScannedRuleFile {
    name: String,
    content: String,
    path: PathBuf,
    apps: Vec<AppType>,
}

/// Read the rules directories of every supported app. Same-named files are
/// merged in `SUPPORTED_APPS` order: content comes from the first one read, and
/// the app list is the union.
fn scan_rule_files() -> Result<Vec<ScannedRuleFile>, AppError> {
    let mut found: BTreeMap<String, ScannedRuleFile> = BTreeMap::new();
    for app in supported_apps() {
        // Discover and install share the same locate step; there is no separate
        // directory table to maintain.
        let Some(support) = rule_adapter(&app).and_then(|adapter| adapter.locate().ok()) else {
            continue;
        };
        for path in list_rule_files(&support.dir, support.extension)? {
            let Some(name) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => {
                    log::warn!("Failed to read rule file {}: {error}", path.display());
                    continue;
                }
            };
            // Discover only offers files that import can actually manage, so the
            // UI never shows a row whose Add would fail validation.
            if let Some(reason) = unimportable_reason(&name, &content) {
                log::warn!("Skipping rule file {}: {reason}", path.display());
                continue;
            }
            found
                .entry(name.clone())
                .and_modify(|file| file.apps.push(app.clone()))
                .or_insert_with(|| ScannedRuleFile {
                    name,
                    content,
                    path,
                    apps: vec![app.clone()],
                });
        }
    }
    Ok(found.into_values().collect())
}

/// Why a discovered file cannot be imported as a rule, if it cannot. Mirrors the
/// validation import applies so Discover never offers a file the import would
/// reject.
fn unimportable_reason(name: &str, content: &str) -> Option<String> {
    if name.trim() != name {
        return Some("file name has leading or trailing whitespace".to_string());
    }
    validate_rule_name(name)
        .err()
        .or_else(|| validate_rule_content(content).err())
        .map(|error| error.to_string())
}

fn all_rules(state: &AppState) -> Result<Vec<Rule>, AppError> {
    Ok(state.db.get_all_rules()?.into_values().collect())
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn not_found(id: &str) -> AppError {
    AppError::localized(
        "rules.not_found",
        format!("Rule 不存在: {id}"),
        format!("Rule not found: {id}"),
    )
}

fn name_taken_error(name: &str) -> AppError {
    AppError::localized(
        "rules.name_taken",
        format!("规则名称已存在: {name}"),
        format!("A rule named \"{name}\" already exists"),
    )
}

/// Names are unique case-insensitively; a rename excludes the rule itself.
fn name_taken(rules: &indexmap::IndexMap<String, Rule>, name: &str, own_id: Option<&str>) -> bool {
    rules.values().any(|rule| {
        if Some(rule.id.as_str()) == own_id {
            return false;
        }
        same_name(&rule.id, name) || same_name(&rule.name, name)
    })
}

fn sync_all_apps(rules: &[Rule], db: &Database) -> Result<(), AppError> {
    let mut first_error = None;
    for app in AppType::all() {
        if let Err(error) = sync_app(&app, rules, db) {
            log::warn!("Failed to sync rules for {}: {error}", app.as_str());
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Template method: installing rules into any app takes the same three steps.
///
/// 1. `locate`: resolve the install location;
/// 2. `render` + write files: copy / generate rule files;
/// 3. `apply_integration`: edit settings so the app reads them; a no-op for
///    natively supported apps.
///
/// If step 3 fails, the files are kept and the next sync retries only the
/// failed step.
fn sync_app(app: &AppType, rules: &[Rule], db: &Database) -> Result<(), AppError> {
    // Skip unsupported or unwired apps so a failing `locate` does not poison the
    // whole sync.
    if !app_supports(app) {
        return Ok(());
    }
    let Some(adapter) = rule_adapter(app) else {
        return Ok(());
    };

    // Step 1: resolve the install location.
    let support = adapter
        .locate()
        .map_err(|error| sync_step_error(app, RuleSyncStep::Locate, error))?;

    // Step 2: copy or generate rule files.
    project_rule_files(app, &support, rules, |rule| adapter.render(rule))
        .map_err(|error| sync_step_error(app, RuleSyncStep::WriteFiles, error))?;

    // Step 3: edit settings; a no-op for natively supported apps.
    let enabled = rules.iter().any(|rule| rule.apps.is_enabled_for(app));
    adapter
        .apply_integration(db, enabled)
        .map_err(|error| sync_step_error(app, RuleSyncStep::Enable, error))?;
    Ok(())
}

fn project_rule_files(
    app: &AppType,
    support: &crate::rules::RuleSupport,
    rules: &[Rule],
    render: impl Fn(&Rule) -> String,
) -> Result<(), AppError> {
    let enabled = rules
        .iter()
        .filter(|rule| rule.apps.is_enabled_for(app))
        .collect::<Vec<_>>();

    let manifest_path = support.dir.join(MANIFEST_FILE);
    let manifest_exists = manifest_path
        .try_exists()
        .map_err(|error| AppError::io(&manifest_path, error))?;
    if enabled.is_empty() && !manifest_exists {
        // Do not conjure a directory for an app whose rules were never enabled.
        return Ok(());
    }

    let previous = read_manifest(&manifest_path)?;
    let managed = previous.iter().map(String::as_str).collect::<HashSet<_>>();
    // A case-only rename is the same file on a case-insensitive filesystem, so
    // the spelling from the old manifest must also count as managed; otherwise
    // it would be mistaken for a user file and reported as a conflict.
    let managed_ci = previous
        .iter()
        .map(|id| id.to_lowercase())
        .collect::<HashSet<_>>();
    let current = enabled
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<HashSet<_>>();

    // Precheck: find every conflict before touching disk, so a conflict leaves
    // the previous projection intact.
    let mut rendered = Vec::with_capacity(enabled.len());
    for rule in &enabled {
        let Some(path) = support.file_path(&rule.id) else {
            return Err(AppError::InvalidInput(format!(
                "Invalid rule id: {}",
                rule.id
            )));
        };
        let content = render(rule);
        let managed_here =
            managed.contains(rule.id.as_str()) || managed_ci.contains(&rule.id.to_lowercase());
        if !managed_here {
            // Unreadable targets (permissions, non-UTF-8, a directory at the
            // path) also count as conflicts: fail rather than overwrite a
            // same-named file of unknown origin.
            match std::fs::read_to_string(&path) {
                Ok(existing) if existing == content => {}
                Ok(_) => return Err(file_conflict(&path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(file_conflict(&path)),
            }
        }
        rendered.push((rule, path, content));
    }

    std::fs::create_dir_all(&support.dir).map_err(|e| AppError::io(&support.dir, e))?;

    for stale_id in &previous {
        if current.contains(stale_id.as_str()) {
            continue;
        }
        let Some(path) = support.file_path(stale_id) else {
            log::warn!("Ignoring unsafe rule id in manifest: {stale_id}");
            continue;
        };
        crate::config::delete_file(&path)?;
    }

    for (_, path, content) in &rendered {
        write_text_if_changed(path, content)?;
    }

    if enabled.is_empty() {
        crate::config::delete_file(&manifest_path)?;
    } else {
        write_manifest(
            &manifest_path,
            &enabled
                .iter()
                .map(|rule| rule.id.clone())
                .collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

fn file_conflict(path: &Path) -> AppError {
    AppError::localized(
        "rules.file_conflict",
        format!("规则文件已存在且不是 CC Switch 管理的：{}", path.display()),
        format!(
            "A rule file already exists and is not managed by CC Switch: {}",
            path.display()
        ),
    )
}

fn write_text_if_changed(path: &Path, text: &str) -> Result<(), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == text => Ok(()),
        _ => crate::config::write_text_file(path, text),
    }
}

fn write_manifest(path: &Path, rules: &[String]) -> Result<(), AppError> {
    let text = serde_json::to_string_pretty(&RuleManifest {
        rules: rules.to_vec(),
    })
    .map_err(|e| AppError::Config(format!("Failed to serialize rule manifest: {e}")))?;
    write_text_if_changed(path, &text)
}

fn sync_step_error(app: &AppType, step: RuleSyncStep, error: AppError) -> AppError {
    let (zh, en) = step.label();
    AppError::localized(
        "rules.sync_step_failed",
        format!(
            "{} 同步规则第 {} 步（{}）失败：{}",
            app.as_str(),
            step.number(),
            zh,
            error
        ),
        format!(
            "Failed to sync rules for {} at step {} ({}): {}",
            app.as_str(),
            step.number(),
            en,
            error
        ),
    )
}

/// Register an existing file in an app's manifest, marking it as managed by CC
/// Switch. Discover → Add uses this to adopt a file before syncing, so it is not
/// mistaken for a same-named user file.
fn adopt_managed_file(app: &AppType, id: &str) -> Result<(), AppError> {
    let Some(adapter) = rule_adapter(app) else {
        return Ok(());
    };
    let support = adapter.locate()?;
    std::fs::create_dir_all(&support.dir).map_err(|e| AppError::io(&support.dir, e))?;
    let manifest_path = support.dir.join(MANIFEST_FILE);
    let mut rules = read_manifest(&manifest_path)?;
    if rules.iter().any(|rule| rule == id) {
        return Ok(());
    }
    rules.push(id.to_string());
    write_manifest(&manifest_path, &rules)
}

fn read_manifest(path: &Path) -> Result<Vec<String>, AppError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str::<RuleManifest>(&text)
            .map(|manifest| {
                manifest
                    .rules
                    .into_iter()
                    .filter(|id| crate::rules::is_safe_rule_id(id))
                    .collect()
            })
            .unwrap_or_else(|error| {
                log::warn!(
                    "Ignoring malformed rule manifest {}: {error}",
                    path.display()
                );
                Vec::new()
            })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(AppError::io(path, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{RuleInjection, RuleSupport};

    fn support(dir: &Path) -> RuleSupport {
        RuleSupport {
            injection: RuleInjection::Native,
            dir: dir.join("rules"),
            extension: "md",
        }
    }

    fn rule(id: &str, content: &str, claude: bool) -> Rule {
        let mut apps = RuleApps::default();
        apps.set_enabled_for(&AppType::Claude, claude);
        Rule {
            id: id.to_string(),
            name: id.to_string(),
            preview: content_preview(content),
            content: content.to_string(),
            apps,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn project(dir: &Path, rules: &[Rule]) -> Result<(), AppError> {
        project_rule_files(&AppType::Claude, &support(dir), rules, |rule| {
            rule.content.clone()
        })
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn projects_enabled_rules_and_records_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let rules = [
            rule("style", "style body", true),
            rule("unused", "x", false),
        ];

        project(dir.path(), &rules).unwrap();

        let rules_dir = support(dir.path()).dir;
        assert_eq!(read(&rules_dir.join("style.md")), "style body");
        assert!(!rules_dir.join("unused.md").exists());
        assert_eq!(
            read_manifest(&rules_dir.join(MANIFEST_FILE)).unwrap(),
            vec!["style".to_string()]
        );
    }

    #[test]
    fn disabling_the_last_rule_removes_only_managed_files() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = support(dir.path()).dir;
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("user.md"), "mine").unwrap();

        project(dir.path(), &[rule("style", "body", true)]).unwrap();
        project(dir.path(), &[]).unwrap();

        assert!(!rules_dir.join("style.md").exists());
        assert!(!rules_dir.join(MANIFEST_FILE).exists());
        assert_eq!(read(&rules_dir.join("user.md")), "mine");
    }

    #[test]
    fn unmanaged_same_name_file_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = support(dir.path()).dir;
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("style.md"), "user content").unwrap();

        assert!(project(dir.path(), &[rule("style", "cc-switch content", true)]).is_err());

        assert_eq!(read(&rules_dir.join("style.md")), "user content");
        assert!(!rules_dir.join(MANIFEST_FILE).exists());
    }

    #[test]
    fn identical_unmanaged_file_is_adopted_without_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = support(dir.path()).dir;
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("style.md"), "same body").unwrap();

        project(dir.path(), &[rule("style", "same body", true)]).unwrap();

        assert_eq!(read(&rules_dir.join("style.md")), "same body");
        assert_eq!(
            read_manifest(&rules_dir.join(MANIFEST_FILE)).unwrap(),
            vec!["style".to_string()]
        );
    }

    #[test]
    fn case_only_rename_replaces_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = support(dir.path()).dir;

        project(dir.path(), &[rule("Style", "one", true)]).unwrap();
        project(dir.path(), &[rule("style", "two", true)]).unwrap();

        assert_eq!(read(&rules_dir.join("style.md")), "two");
        // Compare names instead of `Style.md.exists()`: on a case-insensitive
        // filesystem that check would also see the new `style.md`.
        let mut names = std::fs::read_dir(&rules_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            vec![MANIFEST_FILE.to_string(), "style.md".to_string()]
        );
        assert_eq!(
            read_manifest(&rules_dir.join(MANIFEST_FILE)).unwrap(),
            vec!["style".to_string()]
        );
    }

    #[test]
    fn unsafe_manifest_ids_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        std::fs::write(&path, r#"{"rules":["../escape","/abs","ok"]}"#).unwrap();

        assert_eq!(read_manifest(&path).unwrap(), vec!["ok".to_string()]);
    }

    #[test]
    fn unimportable_files_are_rejected_with_a_reason() {
        assert!(unimportable_reason("Style", "body").is_none());
        assert!(unimportable_reason("TypeScript 风格", "body").is_none());
        assert!(unimportable_reason(" Style", "body").is_some());
        assert!(unimportable_reason("Style ", "body").is_some());
        assert!(unimportable_reason("con", "body").is_some());
        assert!(unimportable_reason("a/b", "body").is_some());
        assert!(unimportable_reason(&"x".repeat(81), "body").is_some());
        assert!(unimportable_reason(
            "style",
            &"x".repeat(crate::rules::MAX_RULE_CONTENT_BYTES + 1)
        )
        .is_some());
    }
}
