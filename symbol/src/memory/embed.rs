//! 向量化抽象：生产环境走 OpenAI 兼容接口，测试可注入确定性实现。

use crate::memory::error::MemoryError;
use model::EmbeddingModel;

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// 输出向量维度。
    fn dimension(&self) -> usize;

    /// 批量向量化，输出顺序与输入一致。
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, MemoryError>;
}

/// 基于 [`EmbeddingModel`] 的实现，并校验输出维度。
pub struct ApiEmbedder {
    model: EmbeddingModel,
    dimension: usize,
}

impl ApiEmbedder {
    pub fn new(model: EmbeddingModel, dimension: usize) -> Self {
        Self { model, dimension }
    }
}

#[async_trait::async_trait]
impl Embedder for ApiEmbedder {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        let vectors = self.model.embed(inputs).await?;
        if let Some(v) = vectors.iter().find(|v| v.len() != self.dimension) {
            return Err(MemoryError::Invalid(format!(
                "embedding dimension {} != configured {}",
                v.len(),
                self.dimension
            )));
        }
        Ok(vectors)
    }
}
