//! 协议中立的工具定义。
//!
//! [`Function`] 只描述一个工具是什么（名称、描述、参数），不含任何厂商 wire 细节。
//! 各协议的 wire 类型（`openai::OpenaiTool`、`anthropic::AnthropicTool`）通过
//! `From<Function>` 把它翻译成自己的请求格式。参数 schema 的公共部分由
//! [`Function::schema_object`] 生成（OpenAI 与 Anthropic 的 JSON Schema 结构一致）。

use serde::{Deserialize, Serialize};

/// 工具函数定义（协议中立）。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Function {
    pub name: String,
    pub description: String,
    pub parameters: Vec<Parameter>,

    /// OpenAI 的严格模式开关；其他协议忽略。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

impl Function {
    /// 生成参数的 JSON Schema 对象：`{ "type": "object", "properties": {...},
    /// "required": [...] }`。OpenAI 与 Anthropic 都用这一份，只是外层包裹不同。
    pub fn schema_object(&self) -> serde_json::Value {
        let mut properties = std::collections::HashMap::<String, serde_json::Value>::new();
        let mut required = Vec::<String>::new();

        for param in &self.parameters {
            let type_str = serde_json::to_value(&param.r#type)
                .unwrap_or_else(|_| serde_json::Value::String("string".to_owned()))
                .as_str()
                .unwrap_or("string")
                .to_owned();

            properties.insert(
                param.name.clone(),
                serde_json::json!({
                    "type": type_str,
                    "description": param.description,
                }),
            );

            if param.required {
                required.push(param.name.clone());
            }
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }
}

/// 工具函数参数定义。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    pub r#type: ParameterType,

    /// 是否必填。默认 true。
    ///
    /// 在 schema 的 `required` 数组中，只有 `required = true` 的参数名会被加入。
    #[serde(skip)]
    pub required: bool,
}

impl Default for Parameter {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            r#type: ParameterType::String,
            required: true,
        }
    }
}

/// 参数类型。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ParameterType {
    #[serde(rename = "string")]
    String,

    #[serde(rename = "integer")]
    Integer,

    #[serde(rename = "number")]
    Number,

    #[serde(rename = "boolean")]
    Boolean,

    #[serde(rename = "array")]
    Array,

    #[serde(rename = "object")]
    Object,
}
