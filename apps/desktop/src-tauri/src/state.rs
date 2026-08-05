//! Application state: the database, the orchestrator, the adapter registry,
//! and the set of currently running tasks.

use commonspace_agents::{AgentAdapter, ClaudeCodeAdapter, CodexCliAdapter};
use commonspace_core::{ProviderId, TaskId};
use commonspace_runtime::{Orchestrator, TaskHandle};
use commonspace_storage::Storage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// Shared application state, managed by Tauri.
pub struct AppState {
    orchestrator: Arc<Orchestrator>,
    adapters: Vec<Box<dyn AgentAdapter>>,
    running: Mutex<HashMap<TaskId, TaskHandle>>,
}

impl AppState {
    /// Open the database in the app data directory and build the runtime.
    pub fn initialize(app: &tauri::AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        let storage = Arc::new(Storage::open(&data_dir.join("commonspace.db"))?);
        let orchestrator = Arc::new(Orchestrator::new(
            Arc::clone(&storage),
            data_dir.join("backups"),
        ));

        Ok(Self {
            orchestrator,
            adapters: vec![Box::new(ClaudeCodeAdapter), Box::new(CodexCliAdapter)],
            running: Mutex::new(HashMap::new()),
        })
    }

    pub fn orchestrator(&self) -> &Arc<Orchestrator> {
        &self.orchestrator
    }

    pub fn storage(&self) -> &Arc<Storage> {
        self.orchestrator.storage()
    }

    pub fn adapters(&self) -> &[Box<dyn AgentAdapter>] {
        &self.adapters
    }

    pub fn adapter(&self, provider: ProviderId) -> Option<&dyn AgentAdapter> {
        self.adapters
            .iter()
            .find(|a| a.id() == provider)
            .map(|a| a.as_ref())
    }

    pub fn track(&self, handle: TaskHandle) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.insert(handle.task_id.clone(), handle);
    }

    pub fn take_running(&self, task_id: &TaskId) -> Option<TaskHandle> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.remove(task_id)
    }
}
