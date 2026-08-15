// Tauri v2 entry point
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    biject_gui_lib::run()
}
