#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() -> eframe::Result<()> {
    // Lance simplement le module de l'interface PC depuis notre librairie hybride !
    wattcoin_wallet::app::run_desktop()
}
