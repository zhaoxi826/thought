/// 记忆器错误。
#[derive(Debug)]
pub enum MemoryError {
    /// SurrealDB 错误。
    Db(surrealdb::Error),

    /// 调用模型（向量化 / 抽取）失败。
    Model(model::Error),

    /// 数据或参数不合法（如空内容、维度不符、存储内容无法解析）。
    Invalid(String),
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::Db(e) => write!(f, "database error: {e}"),
            MemoryError::Model(e) => write!(f, "model error: {e}"),
            MemoryError::Invalid(msg) => write!(f, "invalid: {msg}"),
        }
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MemoryError::Db(e) => Some(e),
            MemoryError::Model(e) => Some(e),
            MemoryError::Invalid(_) => None,
        }
    }
}

impl From<surrealdb::Error> for MemoryError {
    fn from(e: surrealdb::Error) -> Self {
        MemoryError::Db(e)
    }
}

impl From<model::Error> for MemoryError {
    fn from(e: model::Error) -> Self {
        MemoryError::Model(e)
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(e: serde_json::Error) -> Self {
        MemoryError::Invalid(format!("unexpected stored data: {e}"))
    }
}
