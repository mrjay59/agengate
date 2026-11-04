pub mod api;
pub mod bridge;
pub mod config;
pub mod doctor;
pub mod error;
pub mod openai;
pub mod permission;
pub mod provider;
pub mod reasoning;
pub mod run;
pub mod service;
pub mod types;
pub mod cli;

pub type Result<T> = anyhow::Result<T>;

pub use error::AppError;
