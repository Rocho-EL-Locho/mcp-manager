mod assistant;
mod claude_cli;
mod commands;
mod config_read;
mod introspect;
mod logview;
mod mask;
mod metrics;
mod models;
mod parse;
mod preflight;
mod registry;
mod settings;
mod snapshot;
mod stash;
mod toggles;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Desktop-Benachrichtigungen bei Statusverschlechterung (Feature 09).
        .plugin(tauri_plugin_notification::init())
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::check_claude,
            commands::list_servers,
            commands::list_projects,
            commands::delete_project,
            commands::health_check,
            commands::reveal_server_entry,
            commands::introspect_server,
            commands::peek_introspection,
            commands::playground_call,
            commands::get_metrics,
            commands::start_log_session,
            commands::stop_log_session,
            commands::log_session_buffer,
            commands::preflight_server,
            commands::add_server,
            commands::update_server,
            commands::remove_server,
            commands::login_server,
            commands::logout_server,
            commands::reset_project_choices,
            commands::toggle_mcpjson_server,
            commands::toggle_user_server,
            commands::set_scope,
            commands::clone_server,
            commands::list_conflicts,
            commands::rename_server,
            commands::run_claude_assistant,
            commands::search_registry,
            commands::get_settings,
            commands::set_settings,
            commands::create_snapshot,
            commands::list_snapshots,
            commands::restore_snapshot,
            commands::delete_snapshot,
        ])
        .build(tauri::generate_context!())
        .expect("Fehler beim Starten der Tauri-Anwendung")
        // Beim App-Exit alle laufenden Diagnose-Sessions hart beenden (kein
        // zurückbleibender npx-/Serverprozess). Feature 08.
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                app.state::<commands::AppState>().kill_all_log_sessions();
            }
        });
}
