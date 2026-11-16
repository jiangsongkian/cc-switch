//! Rules data access object.
//!
//! Rule content is stored only once (in the database); files in each app's
//! directory are projections. `apps` stores per-app enable flags as JSON, so
//! adding an app needs no schema change.

use indexmap::IndexMap;
use rusqlite::{params, Connection, Row};

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::rules::{content_preview, Rule, RuleApps};

const RULE_COLUMNS: &str = "id, name, content, apps, created_at, updated_at";

const UPSERT_RULE_SQL: &str = "INSERT INTO rules (id, name, content, apps, created_at, updated_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
     ON CONFLICT(id) DO UPDATE SET
        name = excluded.name,
        content = excluded.content,
        apps = excluded.apps,
        updated_at = excluded.updated_at";

fn row_to_rule(row: &Row<'_>) -> rusqlite::Result<Rule> {
    let apps_json: String = row.get(3)?;
    let apps = serde_json::from_str::<RuleApps>(&apps_json).unwrap_or_else(|error| {
        // Silently treating it as "all disabled" would make the startup sync
        // delete existing projection files, so at least log it.
        log::warn!("Ignoring malformed rules.apps JSON: {error}");
        RuleApps::default()
    });
    let content: String = row.get(2)?;
    Ok(Rule {
        id: row.get(0)?,
        name: row.get(1)?,
        preview: content_preview(&content),
        content,
        apps,
        created_at: row.get::<_, i64>(4).unwrap_or(0),
        updated_at: row.get::<_, i64>(5).unwrap_or(0),
    })
}

fn apps_json(rule: &Rule) -> Result<String, AppError> {
    serde_json::to_string(&rule.apps).map_err(|e| AppError::Config(e.to_string()))
}

fn upsert_rule(conn: &Connection, rule: &Rule, apps: &str) -> rusqlite::Result<usize> {
    conn.execute(
        UPSERT_RULE_SQL,
        params![
            rule.id,
            rule.name,
            rule.content,
            apps,
            rule.created_at,
            rule.updated_at
        ],
    )
}

impl Database {
    pub fn get_all_rules(&self) -> Result<IndexMap<String, Rule>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {RULE_COLUMNS} FROM rules ORDER BY name COLLATE NOCASE ASC, id ASC"
            ))
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], row_to_rule)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rules = IndexMap::new();
        for row in rows {
            let rule = row.map_err(|e| AppError::Database(e.to_string()))?;
            rules.insert(rule.id.clone(), rule);
        }
        Ok(rules)
    }

    pub fn get_rule(&self, id: &str) -> Result<Option<Rule>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(&format!("SELECT {RULE_COLUMNS} FROM rules WHERE id = ?1"))
            .map_err(|e| AppError::Database(e.to_string()))?;

        match stmt.query_row([id], row_to_rule) {
            Ok(rule) => Ok(Some(rule)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    pub fn save_rule(&self, rule: &Rule) -> Result<(), AppError> {
        let apps = apps_json(rule)?;
        let conn = lock_conn!(self.conn);
        upsert_rule(&conn, rule, &apps).map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// Write multiple rules atomically: all succeed, or nothing is stored.
    pub fn save_rules(&self, rules: &[Rule]) -> Result<(), AppError> {
        if rules.is_empty() {
            return Ok(());
        }
        let mut conn = lock_conn!(self.conn);
        let transaction = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        for rule in rules {
            let apps = apps_json(rule)?;
            upsert_rule(&transaction, rule, &apps)
                .map_err(|e| AppError::Database(e.to_string()))?;
        }
        transaction
            .commit()
            .map_err(|e| AppError::Database(e.to_string()))
    }

    /// Rename: delete the old id row and insert the new one in one transaction
    /// so a failure midway cannot lose data.
    pub fn rename_rule(&self, old_id: &str, rule: &Rule) -> Result<(), AppError> {
        let apps = apps_json(rule)?;
        let mut conn = lock_conn!(self.conn);
        let transaction = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        transaction
            .execute("DELETE FROM rules WHERE id = ?1", [old_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        upsert_rule(&transaction, rule, &apps).map_err(|e| AppError::Database(e.to_string()))?;
        transaction
            .commit()
            .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn delete_rule(&self, id: &str) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let affected = conn
            .execute("DELETE FROM rules WHERE id = ?1", [id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, content: &str) -> Rule {
        Rule {
            id: id.to_string(),
            name: id.to_string(),
            preview: content_preview(content),
            content: content.to_string(),
            apps: RuleApps {
                claude: true,
                ..Default::default()
            },
            created_at: 1,
            updated_at: 2,
        }
    }

    #[test]
    fn stores_and_reads_a_rule() {
        let db = Database::memory().unwrap();
        db.save_rule(&rule("style", "body")).unwrap();

        let loaded = db.get_rule("style").unwrap().unwrap();
        assert_eq!(loaded.name, "style");
        assert_eq!(loaded.content, "body");
        assert_eq!(loaded.preview, "body");
        assert!(loaded.apps.claude);
        assert_eq!(loaded.created_at, 1);
        assert_eq!(loaded.updated_at, 2);
    }

    #[test]
    fn save_rules_writes_the_whole_batch() {
        let db = Database::memory().unwrap();
        db.save_rules(&[rule("a", "one"), rule("b", "two")])
            .unwrap();

        let all = db.get_all_rules().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["a"].content, "one");
        assert_eq!(all["b"].content, "two");
    }

    #[test]
    fn rename_replaces_the_old_row() {
        let db = Database::memory().unwrap();
        db.save_rule(&rule("old", "body")).unwrap();

        let renamed = rule("new", "body");
        db.rename_rule("old", &renamed).unwrap();

        assert!(db.get_rule("old").unwrap().is_none());
        assert_eq!(db.get_rule("new").unwrap().unwrap().content, "body");
    }

    #[test]
    fn delete_reports_whether_a_row_existed() {
        let db = Database::memory().unwrap();
        db.save_rule(&rule("style", "body")).unwrap();

        assert!(db.delete_rule("style").unwrap());
        assert!(!db.delete_rule("style").unwrap());
    }

    #[test]
    fn malformed_apps_json_disables_every_app() -> Result<(), Box<dyn std::error::Error>> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO rules (id, name, content, apps) VALUES (?1, ?2, ?3, ?4)",
                params!["style", "style", "body", "not json"],
            )?;
        }

        let loaded = db.get_rule("style")?.unwrap();
        assert_eq!(loaded.apps, RuleApps::default());
        Ok(())
    }
}
