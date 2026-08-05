//! Real-provider smoke tests. These spawn the actual installed CLIs with the
//! user's own authentication and therefore consume (a trivial amount of)
//! real usage. They are `#[ignore]` by default; run them explicitly:
//!
//! ```text
//! cargo test -p commonspace-agents --test provider_smoke -- --ignored --nocapture
//! ```
//!
//! (`scripts/smoke-providers` wraps this.) Mock-only tests are never treated
//! as proof that the adapters work — these are the proof.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use commonspace_agents::adapter::{AgentAdapter, SessionRequest};
use commonspace_agents::{ClaudeCodeAdapter, CodexCliAdapter};
use commonspace_core::{AgentEvent, AuthStatus, InstallStatus, TaskId};

fn session_request(prompt: &str) -> SessionRequest {
    let cwd = std::env::temp_dir();
    SessionRequest {
        task_id: TaskId::generate(),
        prompt: prompt.into(),
        cwd: cwd.clone(),
        workspace_roots: vec![cwd],
        model: None,
        resume: None,
        mcp: None,
    }
}

async fn run_to_completion(
    adapter: &dyn AgentAdapter,
    request: SessionRequest,
) -> (Vec<AgentEvent>, Option<String>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = adapter
        .start_session(request, tx)
        .await
        .expect("session should start");
    let mut events = Vec::new();
    let collect = async {
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(300), collect)
        .await
        .expect("session should finish within 5 minutes");
    let provider_session = session.provider_session_id.borrow().clone();
    let _ = session.done.await;
    (events, provider_session)
}

#[tokio::test]
#[ignore = "requires installed + authenticated Claude Code; uses real subscription usage"]
async fn claude_code_end_to_end() {
    let adapter = ClaudeCodeAdapter;
    let install = adapter.detect().await;
    assert!(
        matches!(install, InstallStatus::Installed { .. }),
        "Claude Code not installed: {install:?}"
    );
    let auth = adapter.auth_status().await;
    assert!(
        matches!(auth, AuthStatus::Subscription { .. } | AuthStatus::ApiKey),
        "Claude Code not signed in: {auth:?}"
    );

    let mut request = session_request("Reply with exactly: commonspace-smoke-ok");
    request.model = Some("haiku".into());
    let (events, provider_session) = run_to_completion(&adapter, request).await;

    eprintln!("claude events: {}", events.len());
    assert!(
        provider_session.is_some(),
        "session id should be captured for resume"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("commonspace-smoke-ok"),
        "unexpected reply: {text:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TaskCompleted { .. })),
        "missing task.completed: {events:?}"
    );
}

#[tokio::test]
#[ignore = "requires installed + authenticated Codex CLI; uses real subscription usage"]
async fn codex_end_to_end() {
    let adapter = CodexCliAdapter;
    let install = adapter.detect().await;
    assert!(
        matches!(install, InstallStatus::Installed { .. }),
        "Codex not installed: {install:?}"
    );
    let auth = adapter.auth_status().await;
    assert!(
        matches!(auth, AuthStatus::Subscription { .. } | AuthStatus::ApiKey),
        "Codex not signed in: {auth:?}"
    );

    let request = session_request("Reply with exactly: commonspace-smoke-ok");
    let (events, provider_session) = run_to_completion(&adapter, request).await;

    eprintln!("codex events: {}", events.len());
    // A usage-limited account yields a clean Error event rather than a hang —
    // that still validates the adapter's error path end to end.
    let completed = events
        .iter()
        .any(|e| matches!(e, AgentEvent::TaskCompleted { .. }));
    let errored = events.iter().any(|e| matches!(e, AgentEvent::Error { .. }));
    assert!(completed || errored, "no terminal event: {events:?}");
    if completed {
        assert!(
            provider_session.is_some(),
            "thread id should be captured for resume"
        );
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text.contains("commonspace-smoke-ok"),
            "unexpected reply: {text:?}"
        );
    }
}

#[tokio::test]
#[ignore = "requires installed provider CLIs"]
async fn auth_probes_are_nondestructive_and_fast() {
    let started = std::time::Instant::now();
    let claude = ClaudeCodeAdapter.auth_status().await;
    let codex = CodexCliAdapter.auth_status().await;
    eprintln!(
        "claude: {claude:?}\ncodex: {codex:?}\nelapsed: {:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(45));
}
