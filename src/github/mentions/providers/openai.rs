use serde_json::{Value, json};

use super::{Credentials, FailureKind, Provider, ProviderFailure, http, strict_json_response};

const OPENROUTER_FREE_MODELS: [&str; 2] = [
    "nvidia/nemotron-3-super-120b-a12b:free",
    "qwen/qwen3.8-27b:free",
];

pub(super) fn catalog(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Vec<String>, ProviderFailure> {
    let url = match provider {
        Provider::Groq => "https://api.groq.com/openai/v1/models",
        Provider::Cerebras => "https://api.cerebras.ai/v1/models",
        _ => {
            return Err(ProviderFailure::new(
                FailureKind::Model,
                "provider has no compatible model catalog",
            ));
        }
    };
    let output = http::get(url, "Authorization", &format!("Bearer {}", credentials.key))?;
    let value = serde_json::from_str::<Value>(&output).map_err(|_| {
        ProviderFailure::new(
            FailureKind::InvalidResponse,
            "returned an invalid model catalog",
        )
    })?;
    Ok(value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect())
}

pub(super) fn answer(
    provider: Provider,
    prompt: &str,
    model: &str,
    credentials: &Credentials,
    instructions: &str,
    schema: &Value,
) -> Result<Value, ProviderFailure> {
    let url = match provider {
        Provider::Groq => "https://api.groq.com/openai/v1/chat/completions".to_owned(),
        Provider::Cloudflare => format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/v1/chat/completions",
            credentials.account_id.as_deref().unwrap_or_default()
        ),
        Provider::Cerebras => "https://api.cerebras.ai/v1/chat/completions".to_owned(),
        Provider::OpenRouter => "https://openrouter.ai/api/v1/chat/completions".to_owned(),
        _ => {
            return Err(ProviderFailure::new(
                FailureKind::Model,
                "provider is not OpenAI compatible",
            ));
        }
    };
    let body = request_body(provider, model, prompt, instructions, schema);
    let body = serde_json::to_vec(&body).map_err(|_| {
        ProviderFailure::new(FailureKind::Model, "could not encode the provider request")
    })?;
    let output = http::post(
        &url,
        "Authorization",
        &format!("Bearer {}", credentials.key),
        body,
    )?;
    parse_answer(&output)
}

fn parse_answer(output: &str) -> Result<Value, ProviderFailure> {
    let value = serde_json::from_str::<Value>(output).map_err(|_| {
        ProviderFailure::new(FailureKind::InvalidResponse, "returned an invalid response")
    })?;
    let choice = &value["choices"][0];
    if choice["finish_reason"] == "length" {
        return Err(ProviderFailure::new(
            FailureKind::InvalidResponse,
            "returned a truncated response",
        ));
    }
    let text = choice["message"]["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            ProviderFailure::new(FailureKind::InvalidResponse, "returned an empty response")
        })?;
    strict_json_response(text)
}

fn request_body(
    provider: Provider,
    model: &str,
    prompt: &str,
    instructions: &str,
    schema: &Value,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": format!("{instructions}\n\nRequired response schema:\n{schema}")},
            {"role": "user", "content": prompt}
        ]
    });
    let token_field = if provider == Provider::Cerebras {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    body[token_field] = json!(1_600);
    match provider {
        Provider::Groq | Provider::Cerebras | Provider::OpenRouter => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "koelu_answer",
                    "strict": true,
                    "schema": schema
                }
            });
            if provider == Provider::OpenRouter {
                // Let OpenRouter fail over upstream errors without another client request
                if model == "openrouter/free" {
                    body.as_object_mut().unwrap().remove("model");
                    body["models"] = json!(OPENROUTER_FREE_MODELS);
                }
                body["reasoning"] = json!({"effort": "low"});
                body["temperature"] = json!(0.2);
                body["provider"] = json!({
                    "require_parameters": true,
                    "sort": "latency",
                    "max_price": {"prompt": 0, "completion": 0, "request": 0}
                });
            }
        }
        Provider::Cloudflare => {}
        _ => unreachable!("OpenAI-compatible providers were matched above"),
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape_matches_each_provider_capability() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        });
        for (provider, token_field, structured) in [
            (Provider::Groq, "max_tokens", true),
            (Provider::Cerebras, "max_completion_tokens", true),
            (Provider::Cloudflare, "max_tokens", false),
            (Provider::OpenRouter, "max_tokens", true),
        ] {
            let body = request_body(provider, "model", "prompt", "instructions", &schema);
            assert_eq!(body[token_field], 1_600);
            assert!(
                body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .ends_with(&schema.to_string())
            );
            assert_eq!(body.get("response_format").is_some(), structured);
            assert_eq!(
                body["provider"]["require_parameters"].as_bool(),
                (provider == Provider::OpenRouter).then_some(true)
            );
        }
        let body = request_body(
            Provider::OpenRouter,
            "openrouter/free",
            "prompt",
            "instructions",
            &schema,
        );
        assert!(body.get("model").is_none());
        assert_eq!(body["models"], json!(OPENROUTER_FREE_MODELS));
        assert!(
            OPENROUTER_FREE_MODELS
                .iter()
                .all(|id| id.ends_with(":free"))
        );
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["provider"]["sort"], "latency");
        assert_eq!(
            body["provider"]["max_price"],
            json!({"prompt": 0, "completion": 0, "request": 0})
        );
    }

    #[test]
    fn rejects_empty_truncated_and_invalid_answers() {
        for (content, finish, valid) in [
            (r#"{"answer":"ready"}"#, "stop", true),
            (r#"{"answer":"ready"}"#, "length", false),
            ("", "stop", false),
            (" ", "stop", false),
            ("not JSON", "stop", false),
        ] {
            let output = json!({"choices": [{
                "finish_reason": finish, "message": {"content": content}
            }]});
            assert_eq!(parse_answer(&output.to_string()).is_ok(), valid);
        }
        assert!(parse_answer(r#"{"error":{"code":429}}"#).is_err());
    }
}
