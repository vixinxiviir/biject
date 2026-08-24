use biject::connectors::profiles::{ConnectionProfile, ProfileError};

// ── Profile commands ────────────────────────────────────────────────────────

#[tauri::command]
fn list_profiles() -> Result<Vec<ConnectionProfile>, String> {
    biject::connectors::profiles::list_profiles()
        .map_err(|e: ProfileError| e.to_string())
}

#[tauri::command]
fn save_profile(profile: ConnectionProfile, password: String) -> Result<(), String> {
    biject::connectors::profiles::save_profile(profile, &password)
        .map_err(|e: ProfileError| e.to_string())
}

#[tauri::command]
fn update_profile(profile: ConnectionProfile, password: Option<String>) -> Result<(), String> {
    biject::connectors::profiles::update_profile(profile, password.as_deref())
        .map_err(|e: ProfileError| e.to_string())
}

#[tauri::command]
fn delete_profile(name: String) -> Result<(), String> {
    biject::connectors::profiles::delete_profile(&name)
        .map_err(|e: ProfileError| e.to_string())
}

#[tauri::command]
fn get_profile_password(name: String) -> Result<String, String> {
    biject::connectors::profiles::get_password(&name)
        .map_err(|e: ProfileError| e.to_string())
}

// ── Diff commands ────────────────────────────────────────────────────────────

#[tauri::command]
fn run_diff(
    path1: String,
    path2: String,
    keys: Vec<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    numeric_tolerance_percent: Option<f64>,
) -> Result<serde_json::Value, String> {
    let tolerance = biject::data::Tolerance::resolve(numeric_tolerance, numeric_tolerance_percent)
        .map_err(|e| e.to_string())?;
    biject::data::run_diff(
        &path1,
        &path2,
        &keys,
        exclude_columns.as_deref(),
        only_columns.as_deref(),
        tolerance,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_schema_diff(
    source1: biject::connectors::SourceConfig,
    source2: biject::connectors::SourceConfig,
) -> Result<biject::schema::SchemaDiffResult, String> {
    let label1 = source1.label();
    let label2 = source2.label();
    let df1 = biject::connectors::load_source(&source1)
        .await
        .map_err(|e| e.to_string())?;
    let df2 = biject::connectors::load_source(&source2)
        .await
        .map_err(|e| e.to_string())?;
    // The desktop app holds connection details, so it can read catalogs and
    // show declared types, nullability and defaults just as the CLI does.
    let source_catalog = biject::connectors::read_catalog(&source1).await;
    let target_catalog = biject::connectors::read_catalog(&source2).await;

    biject::schema::run_schema_diff_frames_with_catalog(
        df1,
        df2,
        &label1,
        &label2,
        source_catalog,
        target_catalog,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_source_diff(
    source1: biject::connectors::SourceConfig,
    source2: biject::connectors::SourceConfig,
    keys: Vec<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    numeric_tolerance_percent: Option<f64>,
) -> Result<serde_json::Value, String> {
    let tolerance = biject::data::Tolerance::resolve(numeric_tolerance, numeric_tolerance_percent)
        .map_err(|e| e.to_string())?;
    let label1 = source1.label();
    let label2 = source2.label();
    let df1 = biject::connectors::load_source(&source1)
        .await
        .map_err(|e| e.to_string())?;
    let df2 = biject::connectors::load_source(&source2)
        .await
        .map_err(|e| e.to_string())?;
    biject::data::run_diff_frames(
        df1,
        df2,
        &label1,
        &label2,
        &keys,
        exclude_columns.as_deref(),
        only_columns.as_deref(),
        tolerance,
    )
    .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_profiles, save_profile, update_profile, delete_profile, get_profile_password,
            run_diff, run_schema_diff, run_source_diff,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
