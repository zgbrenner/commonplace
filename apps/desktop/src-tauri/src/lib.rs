//! The Commonspace desktop shell.
//!
//! This layer is deliberately thin: it wires typed Tauri commands to the
//! crates that hold the actual logic, and streams normalized events to the
//! window over a per-task channel. No policy decisions, no filesystem work,
//! and no provider-specific behaviour live here.

mod commands;
mod state;
mod updates;

pub use state::AppState;

/// Build and run the application.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "commonspace=info,warn".into()),
        )
        .init();

    let mut builder = tauri::Builder::default();

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        // Registered first so a second launch focuses the existing window
        // instead of starting a competing instance against the same database.
        builder = builder
            .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
                use tauri::Manager;
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_focus();
                    let _ = window.unminimize();
                }
            }))
            .plugin(tauri_plugin_window_state::Builder::default().build())
            .plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            use tauri::Manager;
            let state = AppState::initialize(app.handle())?;
            // Any task still marked live belongs to a previous process that
            // did not shut down cleanly; fail it with an explanation rather
            // than leaving a spinner that will never resolve.
            match state.orchestrator().recover_after_restart() {
                Ok(recovered) if !recovered.is_empty() => {
                    tracing::info!(count = recovered.len(), "recovered interrupted tasks");
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "crash recovery failed"),
            }
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::provider_health,
            commands::sign_in_instructions,
            commands::list_workspaces,
            commands::create_workspace,
            commands::add_workspace_folder,
            commands::list_conversations,
            commands::list_messages,
            commands::rename_conversation,
            commands::search_history,
            commands::start_task,
            commands::cancel_task,
            commands::resolve_plan_decision,
            commands::answer_permission,
            commands::list_tasks,
            commands::resumable_session,
            commands::list_conversation_attachments,
            commands::list_task_artifacts,
            commands::list_task_events,
            commands::undo_task,
            commands::undo_file_operation,
            commands::open_artifact,
            commands::reveal_artifact,
            commands::open_external_url,
            commands::get_setting,
            commands::set_setting,
            updates::check_for_update,
            updates::install_update,
            updates::open_release_page,
        ])
        .run(tauri::generate_context!())
        // Nothing has a window yet at this point, so there is no UI to show an
        // error in; log it and exit non-zero rather than panicking silently.
        .unwrap_or_else(|error| {
            tracing::error!(%error, "Commonspace failed to start");
            eprintln!("Commonspace failed to start: {error}");
            std::process::exit(1);
        });
}
