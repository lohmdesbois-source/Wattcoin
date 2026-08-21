#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() -> eframe::Result<()> {
    // Lance simplement le module de l'interface qu'on vient de créer !
    wattcoin_wallet::app::run_desktop()
}