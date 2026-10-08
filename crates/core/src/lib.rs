//! Shared contracts for every ClipCut crate: the project model, the render
//! timeline, configuration, rotating API keys, paths, progress and tool lookup.

mod atomic;
pub mod config;
pub mod http;
pub mod keys;
pub mod media;
pub mod model;
pub mod paths;
pub mod progress;
pub mod timeline;
pub mod tools;

#[cfg(test)]
mod test_support;

pub use config::Config;
pub use keys::{KeyPool, KeyStore, ProviderError};
pub use model::*;
pub use paths::Paths;
pub use progress::{Progress, ProgressEvent};
pub use tools::Tools;
