mod app;
pub mod theme;

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum PlatformCommand {
    BeginWindowMove,
    SetMaximized(bool),
    Minimize,
    Close,
    ArmFileDrag(Vec<PathBuf>),
    DisarmFileDrag,
}

pub use app::HoardApp;
