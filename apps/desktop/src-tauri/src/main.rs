// Hide the console window on Windows release builds; Commonspace is a
// desktop application, and a flashing terminal is exactly what it exists to
// spare people.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    commonspace_desktop_lib::run()
}
