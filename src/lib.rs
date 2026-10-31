pub mod api;
pub mod config;
pub mod error;
pub mod provider;
pub mod service;
pub mod types;
pub mod cli;

pub type Result<T> = anyhow::Result<T>;

pub use error::AppError;
