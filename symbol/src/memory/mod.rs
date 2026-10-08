//! 持久化记忆器。
//!
//! - [`store`]：连接、写入、读取 / 删除；[`retrieve`]：三路检索与生命周期；
//! - [`extract`]：LLM 三元组抽取；[`embed`]：向量化抽象；[`text`]：分词与 hash；
//! - [`tools`]：包装成 agent 工具；[`types`] / [`config`] / [`error`]：数据与配置。

pub mod config;
pub mod embed;
pub mod error;
pub mod extract;
pub mod retrieve;
pub mod store;
pub mod text;
pub mod tools;
pub mod types;
