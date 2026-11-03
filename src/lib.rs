//! Library surface for the aic audit CLI scaffold.
//! Exports: CLI runner and public modules; deps: cli, config, types.

pub mod cli;
pub mod aib;
pub mod aggregator;
pub mod config;
pub mod init;
pub mod layers;
pub mod persist;
pub mod pipeline;
pub mod rules;
pub mod types;
mod touched_paths;

pub use cli::run;
pub use aggregator::Aggregator;
pub use layers::aid_dispatch::{AidDispatcher, AidResponse, RealAidDispatcher};
pub use rules::{load_rules_dir, Rule, RuleLayer, RuleSeverity};
pub use types::{AuditContext, AuditRequest, AuditResult, Evidence, Layer, LayerResult, SignalKind, TaskId, Verdict};
