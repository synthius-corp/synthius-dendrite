// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().skip(1).eq(["--telegram-import-worker"]) {
        synthius_dendrite_telegram::network::run_worker();
        return;
    }
    app_lib::run();
}
