#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Le nom exact du projet dans le fichier Cargo.toml (avec des underscores).
    wattcoin_wallet_lib::run();
}