mod assistant;
mod claude_cli;
mod commands;
mod config_read;
mod introspect;
mod mask;
mod models;
mod parse;
mod preflight;
mod settings;
mod stash;
mod toggles;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            commands::run_claude_assistant,
            commands::get_settings,
            commands::set_settings,
        ])
        .run(tauri::generate_context!())
        .expect("Fehler beim Starten der Tauri-Anwendung");
}
