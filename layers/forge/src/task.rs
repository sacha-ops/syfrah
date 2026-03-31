//! Task engine — operation records with status tracking.
//!
//! Every mutation requested through the Forge API creates a Task record.
//! Tasks are stored via [`syfrah_state::LayerDb`] and exposed via the API
//! for observability.

use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

/// Task state machine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
}

/// A task record representing an operation.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Task {
    /// Unique task ID.
    pub id: String,
    /// Resource this task operates on.
    pub resource_id: String,
    /// Operation name (e.g., "create_instance", "delete_instance").
    pub operation: String,
    /// Current state.
    pub state: TaskState,
    /// Unix timestamp of creation.
    pub created_at: u64,
    /// Unix timestamp of completion (if completed or failed).
    pub completed_at: Option<u64>,
    /// Error message (if failed).
    pub error: Option<String>,
}

const TABLE: &str = "tasks";

/// Task store backed by LayerDb.
pub struct TaskStore {
    db: LayerDb,
}

impl TaskStore {
    /// Create a task store using the given LayerDb.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Create a new task.
    pub fn create_task(
        &self,
        id: &str,
        resource_id: &str,
        operation: &str,
    ) -> Result<Task, syfrah_state::StateError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let task = Task {
            id: id.to_string(),
            resource_id: resource_id.to_string(),
            operation: operation.to_string(),
            state: TaskState::Pending,
            created_at: now,
            completed_at: None,
            error: None,
        };

        self.db.set(TABLE, id, &task)?;
        Ok(task)
    }

    /// Mark a task as running.
    pub fn start_task(&self, id: &str) -> Result<Option<Task>, syfrah_state::StateError> {
        self.update_state(id, TaskState::Running, None)
    }

    /// Mark a task as completed.
    pub fn complete_task(&self, id: &str) -> Result<Option<Task>, syfrah_state::StateError> {
        self.update_state(id, TaskState::Completed, None)
    }

    /// Mark a task as failed.
    pub fn fail_task(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<Task>, syfrah_state::StateError> {
        self.update_state(id, TaskState::Failed, Some(error.to_string()))
    }

    /// Get a task by ID.
    pub fn get_task(&self, id: &str) -> Result<Option<Task>, syfrah_state::StateError> {
        self.db.get(TABLE, id)
    }

    /// List all tasks, optionally filtered by resource_id.
    pub fn list_tasks(
        &self,
        resource_id: Option<&str>,
    ) -> Result<Vec<Task>, syfrah_state::StateError> {
        let all: Vec<(String, Task)> = self.db.list(TABLE)?;
        let mut tasks: Vec<Task> = all
            .into_iter()
            .map(|(_, t)| t)
            .filter(|t| {
                if let Some(rid) = resource_id {
                    t.resource_id == rid
                } else {
                    true
                }
            })
            .collect();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(tasks)
    }

    fn update_state(
        &self,
        id: &str,
        new_state: TaskState,
        error: Option<String>,
    ) -> Result<Option<Task>, syfrah_state::StateError> {
        let task: Option<Task> = self.db.get(TABLE, id)?;
        match task {
            Some(mut t) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                t.state = new_state;
                if matches!(t.state, TaskState::Completed | TaskState::Failed) {
                    t.completed_at = Some(now);
                }
                t.error = error;
                self.db.set(TABLE, id, &t)?;
                Ok(Some(t))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, TaskStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = LayerDb::open_at(&dir.path().join("forge_tasks.redb")).unwrap();
        (dir, TaskStore::new(db))
    }

    #[test]
    fn create_and_get_task() {
        let (_dir, store) = temp_store();
        let task = store.create_task("t-1", "vm-1", "create_instance").unwrap();
        assert_eq!(task.state, TaskState::Pending);

        let fetched = store.get_task("t-1").unwrap().unwrap();
        assert_eq!(fetched.resource_id, "vm-1");
    }

    #[test]
    fn complete_task() {
        let (_dir, store) = temp_store();
        store.create_task("t-2", "vm-2", "delete_instance").unwrap();
        let task = store.complete_task("t-2").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert!(task.completed_at.is_some());
    }

    #[test]
    fn fail_task() {
        let (_dir, store) = temp_store();
        store.create_task("t-3", "vm-3", "create_instance").unwrap();
        let task = store.fail_task("t-3", "out of memory").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Failed);
        assert_eq!(task.error.as_deref(), Some("out of memory"));
    }

    #[test]
    fn list_tasks_with_filter() {
        let (_dir, store) = temp_store();
        store.create_task("t-4", "vm-4", "create").unwrap();
        store.create_task("t-5", "vm-5", "create").unwrap();
        store.create_task("t-6", "vm-4", "delete").unwrap();

        let all = store.list_tasks(None).unwrap();
        assert_eq!(all.len(), 3);

        let filtered = store.list_tasks(Some("vm-4")).unwrap();
        assert_eq!(filtered.len(), 2);
    }
}
