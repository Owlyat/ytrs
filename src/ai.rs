//! Generalized AI harness for transcript summarization.
//!
//! Two backends behind one API, both over plain HTTP (no extra deps):
//! - Ollama REST (`/api/tags`, `/api/generate`, default `http://localhost:11434`)
//! - llama.cpp OpenAI-compatible server (`/v1/models`, `/v1/chat/completions`,
//!   default `http://localhost:8080`)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AiBackend {
    #[default]
    Ollama,
    LlamaCpp,
}

#[derive(Debug, Clone, Default)]
pub struct AiConfig {
    pub backend: AiBackend,
    pub model: Option<String>,
    pub endpoint: Option<String>,
}

impl AiConfig {
    pub fn from_cli(cli: &crate::cli::Cli) -> Self {
        Self {
            backend: match cli.ai_backend {
                crate::cli::AiBackendCli::Ollama => AiBackend::Ollama,
                crate::cli::AiBackendCli::LlamaCpp => AiBackend::LlamaCpp,
            },
            model: cli.ai_model.clone(),
            endpoint: cli.ai_endpoint.clone(),
        }
    }

    fn base(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| match self.backend {
                AiBackend::Ollama => "http://localhost:11434".to_string(),
                AiBackend::LlamaCpp => "http://localhost:8080".to_string(),
            })
            .trim_end_matches('/')
            .to_string()
    }

    /// Model names reported by the server (empty when unreachable / none loaded).
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let client = reqwest::Client::new();
        let base = self.base();
        match self.backend {
            AiBackend::Ollama => {
                #[derive(Deserialize)]
                struct Tags {
                    models: Vec<NamedModel>,
                }
                #[derive(Deserialize)]
                struct NamedModel {
                    name: String,
                }
                let tags: Tags = client
                    .get(format!("{base}/api/tags"))
                    .send()
                    .await
                    .context("Ollama server unreachable")?
                    .error_for_status()
                    .context("Ollama /api/tags failed")?
                    .json()
                    .await
                    .context("Ollama /api/tags unreadable")?;
                Ok(tags.models.into_iter().map(|m| m.name).collect())
            }
            AiBackend::LlamaCpp => {
                #[derive(Deserialize)]
                struct ModelList {
                    #[serde(default)]
                    data: Vec<NamedModel>,
                }
                #[derive(Deserialize)]
                struct NamedModel {
                    id: String,
                }
                let list: ModelList = client
                    .get(format!("{base}/v1/models"))
                    .send()
                    .await
                    .context("llama.cpp server unreachable")?
                    .error_for_status()
                    .context("llama.cpp /v1/models failed")?
                    .json()
                    .await
                    .context("llama.cpp /v1/models unreadable")?;
                Ok(list.data.into_iter().map(|m| m.id).collect())
            }
        }
    }

    /// Summarize with an explicit model. Returns the full text (non-streaming).
    pub async fn summarize_with(
        &self,
        model: &str,
        content: &str,
        lang: Option<&str>,
    ) -> Result<String> {
        let prompt = match lang {
            Some(l) => format!(
                "Summarize this content in '{l}' in a few bullet points:\n```{content}```"
            ),
            None => format!("Summarize this content in a few bullet points:\n```{content}```"),
        };
        let client = reqwest::Client::new();
        let base = self.base();
        match self.backend {
            AiBackend::Ollama => {
                #[derive(Serialize)]
                struct GenerateRequest<'a> {
                    model: &'a str,
                    prompt: &'a str,
                    stream: bool,
                }
                #[derive(Deserialize)]
                struct GenerateResponse {
                    response: String,
                }
                let res: GenerateResponse = client
                    .post(format!("{base}/api/generate"))
                    .json(&GenerateRequest {
                        model,
                        prompt: &prompt,
                        stream: false,
                    })
                    .send()
                    .await
                    .context("Ollama server unreachable")?
                    .error_for_status()
                    .context("Ollama /api/generate failed")?
                    .json()
                    .await
                    .context("Ollama /api/generate unreadable")?;
                Ok(res.response)
            }
            AiBackend::LlamaCpp => {
                #[derive(Serialize)]
                struct ChatRequest<'a> {
                    model: &'a str,
                    messages: Vec<ChatMessage<'a>>,
                    stream: bool,
                }
                #[derive(Serialize)]
                struct ChatMessage<'a> {
                    role: &'a str,
                    content: &'a str,
                }
                #[derive(Deserialize)]
                struct ChatResponse {
                    #[serde(default)]
                    choices: Vec<Choice>,
                }
                #[derive(Deserialize)]
                struct Choice {
                    message: ChatMessageOut,
                }
                #[derive(Deserialize)]
                struct ChatMessageOut {
                    content: String,
                }
                let res: ChatResponse = client
                    .post(format!("{base}/v1/chat/completions"))
                    .json(&ChatRequest {
                        model,
                        messages: vec![ChatMessage {
                            role: "user",
                            content: &prompt,
                        }],
                        stream: false,
                    })
                    .send()
                    .await
                    .context("llama.cpp server unreachable")?
                    .error_for_status()
                    .context("llama.cpp /v1/chat/completions failed")?
                    .json()
                    .await
                    .context("llama.cpp /v1/chat/completions unreadable")?;
                res.choices
                    .into_iter()
                    .next()
                    .map(|c| c.message.content)
                    .context("llama.cpp returned no choices")
            }
        }
    }

    /// Summarize with the configured model, else the first one listed.
    /// Returns `(model_used, text)`.
    pub async fn summarize(
        &self,
        content: &str,
        lang: Option<&str>,
    ) -> Result<(String, String)> {
        let model = match self.model.clone() {
            Some(m) => m,
            None => self
                .list_models()
                .await?
                .into_iter()
                .next()
                .context("No AI models available (is the server running?)")?,
        };
        let text = self.summarize_with(&model, content, lang).await?;
        Ok((model, text))
    }
}
