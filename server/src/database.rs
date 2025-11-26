use anyhow::Result;
use rusqlite::{Connection, params};
use std::sync::Mutex;
use dirs;
use serde_json::Value;

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Initialize database connection and create tables if they don't exist
    pub fn new() -> Result<Self> {
        // Get database path in config directory
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
            .join("starthub");
        
        std::fs::create_dir_all(&config_dir)?;
        
        let db_path = config_dir.join("server.db");
        println!("🗄️  SQLite database path: {:?}", db_path);
        let conn = Connection::open(&db_path)?;
        let conn = Mutex::new(conn);
        
        let db = Self { conn };
        db.init_schema()?;
        
        Ok(db)
    }

    /// Initialize database schema
    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        
        // Create actions table matching Supabase schema
        conn.execute(
            "CREATE TABLE IF NOT EXISTS actions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                description TEXT,
                slug TEXT NOT NULL,
                rls_owner_id TEXT,
                git_allowed_repository_id TEXT,
                kind TEXT NOT NULL DEFAULT 'COMPOSITION',
                namespace TEXT,
                download_count INTEGER NOT NULL DEFAULT 0,
                is_sync INTEGER NOT NULL DEFAULT 1,
                latest_action_version_id TEXT,
                FOREIGN KEY (latest_action_version_id) REFERENCES action_versions(id) ON DELETE SET NULL
            )",
            [],
        )?;

        // Create action_versions table matching Supabase schema
        conn.execute(
            "CREATE TABLE IF NOT EXISTS action_versions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                action_id TEXT NOT NULL,
                version_number TEXT NOT NULL,
                commit_sha TEXT,
                manifest TEXT,
                FOREIGN KEY (action_id) REFERENCES actions(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // Create executions table to store execution history
        conn.execute(
            "CREATE TABLE IF NOT EXISTS executions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                action_ref TEXT NOT NULL,
                action_version_id TEXT,
                inputs TEXT NOT NULL,
                outputs TEXT,
                status TEXT NOT NULL,
                error_message TEXT,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (action_version_id) REFERENCES action_versions(id) ON DELETE SET NULL
            )",
            [],
        )?;

        // Create execution_logs table for storing log entries
        conn.execute(
            "CREATE TABLE IF NOT EXISTS execution_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                execution_id INTEGER NOT NULL,
                level TEXT NOT NULL,
                message TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (execution_id) REFERENCES executions(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // Create indexes for better query performance
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_actions_slug ON actions(slug)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_actions_namespace ON actions(namespace)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_actions_rls_owner_id ON actions(rls_owner_id)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_actions_latest_action_version_id ON actions(latest_action_version_id)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_action_versions_action_id ON action_versions(action_id)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_action_versions_version_number ON action_versions(version_number)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_executions_action_ref ON executions(action_ref)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_executions_action_version_id ON executions(action_version_id)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_executions_started_at ON executions(started_at)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_execution_logs_execution_id ON execution_logs(execution_id)",
            [],
        )?;

        // Migration: Add manifest column to action_versions if it doesn't exist
        // SQLite doesn't support IF NOT EXISTS for ALTER TABLE, so we check first
        let table_info: Result<String, rusqlite::Error> = conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='action_versions'",
            [],
            |row| row.get(0),
        );
        
        if let Ok(sql) = table_info {
            if !sql.contains("manifest") {
                conn.execute(
                    "ALTER TABLE action_versions ADD COLUMN manifest TEXT",
                    [],
                )?;
            }
        }

        Ok(())
    }

    /// Store a new execution
    pub fn create_execution(
        &self,
        action_ref: &str,
        inputs: &Value,
        status: &str,
        action_version_id: Option<&str>,
    ) -> Result<i64> {
        let inputs_json = serde_json::to_string(inputs)?;
        let started_at = chrono::Utc::now().to_rfc3339();
        
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO executions (action_ref, action_version_id, inputs, status, started_at) 
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![action_ref, action_version_id, inputs_json, status, started_at],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Update execution with outputs and completion status
    pub fn complete_execution(
        &self,
        execution_id: i64,
        outputs: &Value,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let outputs_json = serde_json::to_string(outputs)?;
        let completed_at = chrono::Utc::now().to_rfc3339();
        
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE executions 
             SET outputs = ?1, status = ?2, error_message = ?3, completed_at = ?4
             WHERE id = ?5",
            params![outputs_json, status, error_message, completed_at, execution_id],
        )?;

        Ok(())
    }

    /// Add a log entry for an execution
    pub fn add_log(
        &self,
        execution_id: i64,
        level: &str,
        message: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO execution_logs (execution_id, level, message) 
             VALUES (?1, ?2, ?3)",
            params![execution_id, level, message],
        )?;

        Ok(())
    }

    /// Get execution history
    pub fn get_executions(
        &self,
        limit: Option<i32>,
        action_ref: Option<&str>,
    ) -> Result<Vec<ExecutionRecord>> {
        let limit = limit.unwrap_or(100);
        let conn = self.conn.lock().unwrap();
        
        let mut executions = Vec::new();
        
        if let Some(ref_filter) = action_ref {
            let mut stmt = conn.prepare(
                "SELECT id, action_ref, inputs, outputs, status, error_message, started_at, completed_at, created_at 
                 FROM executions 
                 WHERE action_ref = ?1 
                 ORDER BY started_at DESC 
                 LIMIT ?2"
            )?;
            let rows = stmt.query_map(params![ref_filter, limit], |row| {
                Ok(ExecutionRecord {
                    id: row.get(0)?,
                    action_ref: row.get(1)?,
                    inputs: row.get::<_, String>(2)?.parse().unwrap_or(Value::Null),
                    outputs: row.get::<_, Option<String>>(3)?
                        .map(|s| s.parse().unwrap_or(Value::Null))
                        .unwrap_or(Value::Null),
                    status: row.get(4)?,
                    error_message: row.get(5)?,
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })?;
            
            for row in rows {
                executions.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, action_ref, inputs, outputs, status, error_message, started_at, completed_at, created_at 
                 FROM executions 
                 ORDER BY started_at DESC 
                 LIMIT ?1"
            )?;
            let rows = stmt.query_map(params![limit], |row| {
                Ok(ExecutionRecord {
                    id: row.get(0)?,
                    action_ref: row.get(1)?,
                    inputs: row.get::<_, String>(2)?.parse().unwrap_or(Value::Null),
                    outputs: row.get::<_, Option<String>>(3)?
                        .map(|s| s.parse().unwrap_or(Value::Null))
                        .unwrap_or(Value::Null),
                    status: row.get(4)?,
                    error_message: row.get(5)?,
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })?;
            
            for row in rows {
                executions.push(row?);
            }
        }

        Ok(executions)
    }

    /// Get logs for a specific execution
    pub fn get_execution_logs(&self, execution_id: i64) -> Result<Vec<LogRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, execution_id, level, message, timestamp 
             FROM execution_logs 
             WHERE execution_id = ?1 
             ORDER BY timestamp ASC"
        )?;

        let rows = stmt.query_map(params![execution_id], |row| {
            Ok(LogRecord {
                id: row.get(0)?,
                execution_id: row.get(1)?,
                level: row.get(2)?,
                message: row.get(3)?,
                timestamp: row.get(4)?,
            })
        })?;

        let mut logs = Vec::new();
        for row in rows {
            logs.push(row?);
        }

        Ok(logs)
    }

    /// Upsert an action (insert or update)
    pub fn upsert_action(
        &self,
        id: &str,
        slug: &str,
        description: Option<&str>,
        rls_owner_id: Option<&str>,
        git_allowed_repository_id: Option<&str>,
        kind: &str,
        namespace: Option<&str>,
        latest_action_version_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO actions (id, slug, description, rls_owner_id, git_allowed_repository_id, kind, namespace, latest_action_version_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                slug = excluded.slug,
                description = excluded.description,
                rls_owner_id = excluded.rls_owner_id,
                git_allowed_repository_id = excluded.git_allowed_repository_id,
                kind = excluded.kind,
                namespace = excluded.namespace,
                latest_action_version_id = excluded.latest_action_version_id",
            params![id, slug, description, rls_owner_id, git_allowed_repository_id, kind, namespace, latest_action_version_id],
        )?;
        Ok(())
    }

    /// Upsert an action version
    /// Automatically updates the action's latest_action_version_id to point to the most recent version
    pub fn upsert_action_version(
        &self,
        id: &str,
        action_id: &str,
        version_number: &str,
        commit_sha: Option<&str>,
        manifest: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO action_versions (id, action_id, version_number, commit_sha, manifest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                action_id = excluded.action_id,
                version_number = excluded.version_number,
                commit_sha = excluded.commit_sha,
                manifest = excluded.manifest",
            params![id, action_id, version_number, commit_sha, manifest],
        )?;
        
        // Update the action's latest_action_version_id to point to the most recent version
        // This ensures it always points to the version with the latest created_at timestamp
        conn.execute(
            "UPDATE actions 
             SET latest_action_version_id = (
                 SELECT id FROM action_versions 
                 WHERE action_id = ?1 
                 ORDER BY created_at DESC, id DESC 
                 LIMIT 1
             )
             WHERE id = ?1",
            params![action_id],
        )?;
        
        Ok(())
    }

    /// Get an action by id
    pub fn get_action(&self, id: &str) -> Result<Option<ActionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, description, slug, rls_owner_id, git_allowed_repository_id, kind, namespace, download_count, is_sync, latest_action_version_id
             FROM actions
             WHERE id = ?1"
        )?;

        let mut rows = stmt.query_map(params![id], |row| {
            Ok(ActionRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                description: row.get(2)?,
                slug: row.get(3)?,
                rls_owner_id: row.get(4)?,
                git_allowed_repository_id: row.get(5)?,
                kind: row.get(6)?,
                namespace: row.get(7)?,
                download_count: row.get(8)?,
                is_sync: row.get::<_, i64>(9)? != 0,
                latest_action_version_id: row.get(10)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Helper function to map a row to ActionRecord
    fn map_action_record(row: &rusqlite::Row) -> rusqlite::Result<ActionRecord> {
        Ok(ActionRecord {
            id: row.get(0)?,
            created_at: row.get(1)?,
            description: row.get(2)?,
            slug: row.get(3)?,
            rls_owner_id: row.get(4)?,
            git_allowed_repository_id: row.get(5)?,
            kind: row.get(6)?,
            namespace: row.get(7)?,
            download_count: row.get(8)?,
            is_sync: row.get::<_, i64>(9)? != 0,
            latest_action_version_id: row.get(10)?,
        })
    }

    /// Get an action by namespace and slug
    pub fn get_action_by_namespace_slug(&self, namespace: &str, slug: &str) -> Result<Option<ActionRecord>> {
        let conn = self.conn.lock().unwrap();
        
        // Handle NULL namespace (empty string or "null" means NULL in database)
        if namespace.is_empty() || namespace == "null" {
            let mut stmt = conn.prepare(
                "SELECT id, created_at, description, slug, rls_owner_id, git_allowed_repository_id, kind, namespace, download_count, is_sync, latest_action_version_id
                 FROM actions
                 WHERE (namespace IS NULL OR namespace = '') AND slug = ?1"
            )?;
            let mut rows = stmt.query_map(params![slug], Self::map_action_record)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, created_at, description, slug, rls_owner_id, git_allowed_repository_id, kind, namespace, download_count, is_sync, latest_action_version_id
                 FROM actions
                 WHERE namespace = ?1 AND slug = ?2"
            )?;
            let mut rows = stmt.query_map(params![namespace, slug], Self::map_action_record)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        }
    }

    /// Get action versions for an action
    pub fn get_action_versions(&self, action_id: &str) -> Result<Vec<ActionVersionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, action_id, version_number, commit_sha, manifest
             FROM action_versions
             WHERE action_id = ?1
             ORDER BY created_at DESC"
        )?;

        let rows = stmt.query_map(params![action_id], |row| {
            Ok(ActionVersionRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                action_id: row.get(2)?,
                version_number: row.get(3)?,
                commit_sha: row.get(4)?,
                manifest: row.get(5)?,
            })
        })?;

        let mut versions = Vec::new();
        for row in rows {
            versions.push(row?);
        }

        Ok(versions)
    }

    /// Get latest action version for an action
    pub fn get_latest_action_version(&self, action_id: &str) -> Result<Option<ActionVersionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, action_id, version_number, commit_sha, manifest
             FROM action_versions
             WHERE action_id = ?1
             ORDER BY created_at DESC
             LIMIT 1"
        )?;

        let mut rows = stmt.query_map(params![action_id], |row| {
            Ok(ActionVersionRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                action_id: row.get(2)?,
                version_number: row.get(3)?,
                commit_sha: row.get(4)?,
                manifest: row.get(5)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Increment download count for an action
    pub fn increment_download_count(&self, action_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE actions SET download_count = download_count + 1 WHERE id = ?1",
            params![action_id],
        )?;
        Ok(())
    }

    /// Get all actions with their latest action version joined
    pub fn get_actions_with_latest_version(
        &self,
        limit: Option<i32>,
        namespace: Option<&str>,
    ) -> Result<Vec<ActionWithVersion>> {
        let limit = limit.unwrap_or(100);
        let conn = self.conn.lock().unwrap();
        
        let mut actions = Vec::new();
        
        if let Some(ns) = namespace {
            let mut stmt = conn.prepare(
                "SELECT 
                    a.id, a.created_at, a.description, a.slug, a.rls_owner_id, 
                    a.git_allowed_repository_id, a.kind, a.namespace, a.download_count, 
                    a.is_sync, a.latest_action_version_id,
                    av.id, av.created_at, av.action_id, av.version_number, av.commit_sha, av.manifest
                 FROM actions a
                 LEFT JOIN action_versions av ON a.latest_action_version_id = av.id
                 WHERE a.namespace = ?1
                 ORDER BY a.created_at DESC
                 LIMIT ?2"
            )?;
            
            let rows = stmt.query_map(params![ns, limit], |row| {
                let version_id: Option<String> = row.get(11)?;
                let latest_version = if version_id.is_some() {
                    Some(ActionVersionRecord {
                        id: row.get(11)?,
                        created_at: row.get(12)?,
                        action_id: row.get(13)?,
                        version_number: row.get(14)?,
                        commit_sha: row.get(15)?,
                        manifest: row.get(16)?,
                    })
                } else {
                    None
                };
                
                Ok(ActionWithVersion {
                    action: ActionRecord {
                        id: row.get(0)?,
                        created_at: row.get(1)?,
                        description: row.get(2)?,
                        slug: row.get(3)?,
                        rls_owner_id: row.get(4)?,
                        git_allowed_repository_id: row.get(5)?,
                        kind: row.get(6)?,
                        namespace: row.get(7)?,
                        download_count: row.get(8)?,
                        is_sync: row.get::<_, i64>(9)? != 0,
                        latest_action_version_id: row.get(10)?,
                    },
                    latest_version,
                })
            })?;
            
            for row in rows {
                actions.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT 
                    a.id, a.created_at, a.description, a.slug, a.rls_owner_id, 
                    a.git_allowed_repository_id, a.kind, a.namespace, a.download_count, 
                    a.is_sync, a.latest_action_version_id,
                    av.id, av.created_at, av.action_id, av.version_number, av.commit_sha, av.manifest
                 FROM actions a
                 LEFT JOIN action_versions av ON a.latest_action_version_id = av.id
                 ORDER BY a.created_at DESC
                 LIMIT ?1"
            )?;
            
            let rows = stmt.query_map(params![limit], |row| {
                let version_id: Option<String> = row.get(11)?;
                let latest_version = if version_id.is_some() {
                    Some(ActionVersionRecord {
                        id: row.get(11)?,
                        created_at: row.get(12)?,
                        action_id: row.get(13)?,
                        version_number: row.get(14)?,
                        commit_sha: row.get(15)?,
                        manifest: row.get(16)?,
                    })
                } else {
                    None
                };
                
                Ok(ActionWithVersion {
                    action: ActionRecord {
                        id: row.get(0)?,
                        created_at: row.get(1)?,
                        description: row.get(2)?,
                        slug: row.get(3)?,
                        rls_owner_id: row.get(4)?,
                        git_allowed_repository_id: row.get(5)?,
                        kind: row.get(6)?,
                        namespace: row.get(7)?,
                        download_count: row.get(8)?,
                        is_sync: row.get::<_, i64>(9)? != 0,
                        latest_action_version_id: row.get(10)?,
                    },
                    latest_version,
                })
            })?;
            
            for row in rows {
                actions.push(row?);
            }
        }

        Ok(actions)
    }

}

#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    pub id: i64,
    pub action_ref: String,
    pub inputs: Value,
    pub outputs: Value,
    pub status: String,
    pub error_message: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct LogRecord {
    pub id: i64,
    pub execution_id: i64,
    pub level: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub id: String,
    pub created_at: String,
    pub description: Option<String>,
    pub slug: String,
    pub rls_owner_id: Option<String>,
    pub git_allowed_repository_id: Option<String>,
    pub kind: String,
    pub namespace: Option<String>,
    pub download_count: i64,
    pub is_sync: bool,
    pub latest_action_version_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActionVersionRecord {
    pub id: String,
    pub created_at: String,
    pub action_id: String,
    pub version_number: String,
    pub commit_sha: Option<String>,
    pub manifest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActionWithVersion {
    pub action: ActionRecord,
    pub latest_version: Option<ActionVersionRecord>,
}

