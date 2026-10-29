//! Embeddings and image descriptions via a local Ollama server. Optional:
//! every FDS feature except vectors and image description works without it.

use serde::{Deserialize, Serialize};

pub fn base_url() -> String {
    std::env::var("FDS_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".to_string())
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

pub async fn embed(model: &str, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let client = reqwest::Client::new();
    let mut out = Vec::with_capacity(texts.len());
    for batch in texts.chunks(32) {
        let resp = client
            .post(format!("{}/api/embed", base_url()))
            .json(&EmbedRequest { model, input: batch })
            .send()
            .await
            .map_err(|e| format!("Ollama not reachable at {} ({e}). Is it running?", base_url()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Ollama returned {status}: {body}"));
        }
        let parsed: EmbedResponse = resp.json().await.map_err(|e| format!("bad Ollama response: {e}"))?;
        if parsed.embeddings.len() != batch.len() {
            return Err("Ollama returned a different number of embeddings than requested".into());
        }
        out.extend(parsed.embeddings);
    }
    Ok(out)
}

/// Embeds one query, or returns None if that isn't possible. Queries degrade
/// to full-text search rather than failing.
pub async fn embed_query(model: &str, text: &str) -> Option<Vec<f32>> {
    embed(model, &[text.to_string()]).await.ok().and_then(|mut v| v.pop())
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    images: Vec<String>,
    stream: bool,
    options: serde_json::Value,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

/// Asks a vision model to describe one image. Used when a record is
/// *written*, never to answer a query: it compiles an image into text the
/// way `embed` compiles text into a vector.
pub async fn describe_image(model: &str, image: &[u8], prompt: &str) -> Result<String, String> {
    use base64::Engine as _;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/generate", base_url()))
        .json(&GenerateRequest {
            model,
            prompt,
            images: vec![base64::engine::general_purpose::STANDARD.encode(image)],
            stream: false,
            // Deterministic: the same image gives the same description.
            options: serde_json::json!({"temperature": 0, "seed": 0}),
        })
        .send()
        .await
        .map_err(|e| format!("Ollama not reachable at {} ({e}). Is it running?", base_url()))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Ollama returned {status}: {body}"));
    }
    let parsed: GenerateResponse = resp.json().await.map_err(|e| format!("bad Ollama response: {e}"))?;
    let text = parsed.response.trim().to_string();
    if text.is_empty() {
        return Err(format!("`{model}` returned an empty description"));
    }
    Ok(text)
}

