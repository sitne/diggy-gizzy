use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct ZaiChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ZaiRequest {
    model: String,
    messages: Vec<ZaiChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ZaiChoice {
    message: ZaiMessage,
}

#[derive(Deserialize)]
struct ZaiMessage {
    content: String,
}

#[derive(Deserialize)]
struct ZaiResponse {
    choices: Vec<ZaiChoice>,
}

pub struct Summarizer {
    api_key: String,
    client: Client,
}

impl Summarizer {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
        }
    }

    pub async fn summarize_meeting(
        &self,
        transcript: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let prompt = format!(
            "以下の会議の文字起こしテキストから、議事録を作成してください。\n\n\
            以下の形式で出力してください:\n\
            📋 **会議概要**\n\
            [簡潔な会議の要約（3-5行）]\n\n\
            👥 **参加者**\n\
            [発言者一覧]\n\n\
            💬 **主な議論内容**\n\
            - [議題1]: [要点]\n\
            - [議題2]: [要点]\n\n\
            ✅ **決定事項**\n\
            - [決定1]\n\
            - [決定2]\n\n\
            📌 **アクションアイテム**\n\
            - [担当]: [タスク内容]\n\n\
            ---\n\
            文字起こしテキスト:\n\
            {}",
            transcript
        );

        let request = ZaiRequest {
            model: "glm-4.7-flash".to_string(),
            messages: vec![
                ZaiChatMessage {
                    role: "system".to_string(),
                    content: "あなたはプロの会議議事録作成者です。与えられた文字起こしテキストから、構造化された議事録を作成してください。日本語で回答してください。".to_string(),
                },
                ZaiChatMessage {
                    role: "user".to_string(),
                    content: prompt,
                },
            ],
            temperature: 0.7,
            max_tokens: 4096,
        };

        let response = self
            .client
            .post("https://api.z.ai/api/paas/v4/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("z.ai API error: {} - {}", status, text).into());
        }

        let zai_response: ZaiResponse = response.json().await?;
        
        if let Some(choice) = zai_response.choices.first() {
            Ok(choice.message.content.clone())
        } else {
            Err("No response from z.ai API".into())
        }
    }

    pub async fn summarize_short(
        &self,
        transcript: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let prompt = format!(
            "以下のテキストを簡潔に要約してください（200文字以内）:\n\n{}",
            transcript
        );

        let request = ZaiRequest {
            model: "glm-4.7-flash".to_string(),
            messages: vec![
                ZaiChatMessage {
                    role: "system".to_string(),
                    content: "簡潔な要約を作成してください。日本語で回答してください。".to_string(),
                },
                ZaiChatMessage {
                    role: "user".to_string(),
                    content: prompt,
                },
            ],
            temperature: 0.5,
            max_tokens: 512,
        };

        let response = self
            .client
            .post("https://api.z.ai/api/paas/v4/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("z.ai API error: {} - {}", status, text).into());
        }

        let zai_response: ZaiResponse = response.json().await?;
        
        if let Some(choice) = zai_response.choices.first() {
            Ok(choice.message.content.clone())
        } else {
            Err("No response from z.ai API".into())
        }
    }
}
