// Pas de console sous Windows en release : c'est une fenêtre.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ki_mobile_lib::run()
}
