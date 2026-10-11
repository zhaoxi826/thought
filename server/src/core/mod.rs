//! 后端内核：认证、存储、错误类型、共享状态。

pub mod auth;
pub mod budget;
pub mod ratelimit;
pub mod config_store;
pub mod db;
pub mod error;
pub mod state;
pub mod tasks;
