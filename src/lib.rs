pub mod analyze;
pub mod attach;
pub mod cli;
pub mod compare;
pub mod contract;
pub mod doctor;
pub mod process;
pub mod report;
pub mod run;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SCHEMA_VERSION: u32 = 1;
