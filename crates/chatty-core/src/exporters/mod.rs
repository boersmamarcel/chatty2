pub mod atif_exporter;
pub mod jsonl_exporter;
pub mod run_export;
pub mod types;

pub use atif_exporter::{export_swarm, swarm_to_atif, swarm_tree_from_atif};
pub use run_export::{ExportRun, export_run};

/// `<config dir>/chatty/exports`: where both frontends write a conversation's
/// export. `None` when the platform has no config directory.
pub fn exports_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|config| config.join("chatty").join("exports"))
}
