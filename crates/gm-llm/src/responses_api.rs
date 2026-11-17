use crate::client::{ChatCompletionResponse, ChatMessage, LlmResponse};
use gm_core::error::{GmError, Result};
use tokio::sync::mpsc;

pub fn serialize_responses_input(
    messages: &[ChatMessage],
    allow_vision: bool,
) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|msg| {
            let mut content = vec![serde_json::json!({
                "type": "input_text",
                "text": msg.content,
            })];
            if allow_vision {
                for img in &msg.images {
                    content.push(serde_json::json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", img.mime_type, img.base64_data),
                        "detail": img.detail.as_deref().unwrap_or("high"),
                    }));
                }
            }
            serde_json::json!({
                "role": msg.role,
                "content": content,
            })
        })
        .collect()
}

pub fn build_responses_body(
    model: &str,
    messages: &[ChatMessage],
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    stream: bool,
    allow_vision: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "input": serialize_responses_input(messages, allow_vision),
        "stream": stream,
    });
    if let Some(mt) = max_tokens
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("max_output_tokens".into(), serde_json::json!(mt));
    }
    if let Some(temp) = temperature
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("temperature".into(), serde_json::json!(temp));
    }
    body
}

pub fn extract_responses_text(val: &serde_json::Value) -> String {
    if let Some(s) = val.get("output_text").and_then(|v| v.as_str())
        && !s.is_empty()
    {
        return s.to_string();
    }
    let mut collected = String::new();
    if let Some(outputs) = val.get("output").and_then(|v| v.as_array()) {
        for item in outputs {
            if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                for part in parts {
                    if let Some(txt) = part.get("text").and_then(|v| v.as_str()) {
                        collected.push_str(txt);
                    }
                }
            }
        }
    }
    collected
}

pub fn parse_responses_json(
    val: &serde_json::Value,
    fallback_model: &str,
    estimated_cost_usd: f64,
) -> ChatCompletionResponse {
    let content = extract_responses_text(val);
    let prompt_tokens = val["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
    let completion_tokens = val["usage"]["output_tokens"]
        .as_u64()
        .unwrap_or(content.len() as u64) as u32;
    ChatCompletionResponse {
        id: val["id"].as_str().unwrap_or("resp_responses").to_string(),
        content,
        model: val["model"].as_str().unwrap_or(fallback_model).to_string(),
        prompt_tokens,
        completion_tokens,
        estimated_cost_usd,
    }
}

pub async fn parse_responses_sse_stream(
    mut resp: reqwest::Response,
    fallback_model: &str,
    messages: &[ChatMessage],
    delta_tx: mpsc::Sender<String>,
    cost_fn: impl FnOnce(u32, u32) -> f64,
) -> Result<LlmResponse> {
    let mut full_text = String::new();
    let mut buffer = Vec::<u8>::new();
    let mut finished = false;
    let mut response_id = String::from("resp_stream");
    let mut parsed_prompt_tokens: Option<u32> = None;
    let mut parsed_completion_tokens: Option<u32> = None;
    let mut current_event = String::new();

    'stream: while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| GmError::Provider(format!("Interrupted Responses stream: {e}")))?
    {
        buffer.extend_from_slice(&chunk);
        if buffer.len() > 2 * 1024 * 1024 {
            return Err(GmError::Provider(
                "Responses stream frame exceeds limit".into(),
            ));
        }
        while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
            let line = std::str::from_utf8(&buffer[..pos])
                .map_err(|_| GmError::Provider("Invalid UTF-8 in Responses stream".into()))?
                .trim()
                .to_string();
            buffer.drain(..pos + 1);

            if line.is_empty() {
                current_event.clear();
                continue;
            }
            if let Some(ev) = line.strip_prefix("event:") {
                current_event = ev.trim().to_string();
                continue;
            }
            if let Some(payload) = line.strip_prefix("data:") {
                let data = payload.trim();
                if data == "[DONE]" {
                    finished = true;
                    break 'stream;
                }
                let val: serde_json::Value = serde_json::from_str(data)
                    .map_err(|e| GmError::Provider(format!("Malformed Responses SSE JSON: {e}")))?;
                let ev_type = val["type"].as_str().unwrap_or(current_event.as_str());
                if ev_type == "error" || ev_type == "response.failed" || val.get("error").is_some()
                {
                    let msg = val["error"]["message"]
                        .as_str()
                        .or_else(|| val["message"].as_str())
                        .unwrap_or("Responses stream returned an error");
                    return Err(GmError::Provider(msg.to_string()));
                }
                if ev_type == "response.output_text.delta"
                    && let Some(delta) = val["delta"].as_str()
                    && !delta.is_empty()
                {
                    if full_text.len() + delta.len() > 8 * 1024 * 1024 {
                        return Err(GmError::Provider(
                            "Responses output exceeds 8 MiB limit".into(),
                        ));
                    }
                    delta_tx
                        .send(delta.to_string())
                        .await
                        .map_err(|_| GmError::Provider("Stream consumer disconnected".into()))?;
                    full_text.push_str(delta);
                } else if ev_type == "response.completed" || ev_type == "response.done" {
                    finished = true;
                    if let Some(id) = val["response"]["id"].as_str() {
                        response_id = id.to_string();
                    }
                    if let Some(u) = val["response"].get("usage").or_else(|| val.get("usage")) {
                        if let Some(it) = u["input_tokens"].as_u64() {
                            parsed_prompt_tokens = Some(it as u32);
                        }
                        if let Some(ot) = u["output_tokens"].as_u64() {
                            parsed_completion_tokens = Some(ot as u32);
                        }
                    }
                    if full_text.is_empty() {
                        let extracted = extract_responses_text(&val["response"]);
                        if !extracted.is_empty() {
                            delta_tx.send(extracted.clone()).await.map_err(|_| {
                                GmError::Provider("Stream consumer disconnected".into())
                            })?;
                            full_text = extracted;
                        }
                    }
                }
            }
        }
    }

    if !finished {
        return Err(GmError::Provider(
            "Responses stream ended before response.completed".into(),
        ));
    }

    let completion_tokens =
        parsed_completion_tokens.unwrap_or(full_text.len().min(u32::MAX as usize) as u32);
    let prompt_tokens = parsed_prompt_tokens.unwrap_or_else(|| {
        messages
            .iter()
            .map(|m| m.content.len().saturating_add(m.images.len() * 8192))
            .sum::<usize>()
            .min(u32::MAX as usize) as u32
    });
    let estimated_cost_usd = cost_fn(prompt_tokens, completion_tokens);
    Ok(ChatCompletionResponse {
        id: response_id,
        content: full_text,
        model: fallback_model.to_string(),
        prompt_tokens,
        completion_tokens,
        estimated_cost_usd,
    })
}
