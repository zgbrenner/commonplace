//! Application state: the database, the orchestrator, the adapter registry,
//! and the set of currently running tasks.

use commonspace_agents::{AgentAdapter, ClaudeCodeAdapter, CodexCliAdapter};
use commonspace_core::{AuthStatus, ProviderId, TaskId};
use commonspace_runtime::{Orchestrator, TaskHandle};
use commonspace_storage::Storage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

/// How long a "signed in" probe answer is trusted before re-checking.
const AUTH_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Shared application state, managed by Tauri.
///
/// Adapters live behind `Arc` because the orchestrator's two-phase flow
/// holds one across a task's whole life (plan pump, execution, revision),
/// which outlives any single command invocation.
pub struct AppState {
    orchestrator: Arc<Orchestrator>,
    adapters: Vec<Arc<dyn AgentAdapter>>,
    running: Mutex<HashMap<TaskId, TaskHandle>>,
    auth_cache: Mutex<HashMap<ProviderId, (AuthStatus, Instant)>>,
}

impl AppState {
    /// Open the database in the app data directory and build the runtime.
    pub fn initialize(app: &tauri::AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        let storage = Arc::new(Storage::open(&data_dir.join("commonspace.db"))?);
        let orchestrator = Arc::new(Orchestrator::new(Arc::clone(&storage), data_dir));

        Ok(Self {
            orchestrator,
            adapters: vec![Arc::new(ClaudeCodeAdapter), Arc::new(CodexCliAdapter)],
            running: Mutex::new(HashMap::new()),
            auth_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Auth status for `adapter`, served from cache when a recent probe found
    /// the connection usable. Probing spawns the provider's CLI, so probing
    /// on every message adds seconds before the task even starts — and one
    /// more short-lived process per message that must keep its console
    /// hidden on Windows. An *unusable* cached answer is never served:
    /// signing in must take effect on the next message, not after a timeout.
    pub async fn auth_status_cached(&self, adapter: &dyn AgentAdapter) -> AuthStatus {
        {
            let cache = self.auth_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((status, probed_at)) = cache.get(&adapter.id()) {
                if probed_at.elapsed() < AUTH_CACHE_TTL && auth_is_usable(status) {
                    return status.clone();
                }
            }
        }
        let status = adapter.auth_status().await;
        self.record_auth_status(adapter.id(), &status);
        status
    }

    /// Record a fresh probe result (also called by the Connections screen's
    /// refresh, so an explicit re-check updates what tasks rely on).
    pub fn record_auth_status(&self, provider: ProviderId, status: &AuthStatus) {
        let mut cache = self.auth_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(provider, (status.clone(), Instant::now()));
    }

    pub fn orchestrator(&self) -> &Arc<Orchestrator> {
        &self.orchestrator
    }

    pub fn storage(&self) -> &Arc<Storage> {
        self.orchestrator.storage()
    }

    pub fn adapters(&self) -> &[Arc<dyn AgentAdapter>] {
        &self.adapters
    }

    pub fn adapter(&self, provider: ProviderId) -> Option<Arc<dyn AgentAdapter>> {
        self.adapters.iter().find(|a| a.id() == provider).cloned()
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

/// Whether a status lets a task start without re-checking.
fn auth_is_usable(status: &AuthStatus) -> bool {
    matches!(
        status,
        AuthStatus::Subscription { .. } | AuthStatus::ApiKey | AuthStatus::LocalModel
    )
}
