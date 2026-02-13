use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Deserialize, Debug)]
struct DeepLResponse {
    translations: Vec<DeepLTranslation>,
}

#[derive(Deserialize, Debug)]
struct DeepLTranslation {
    text: String,
    #[allow(dead_code)]
    detected_source_language: Option<String>,
}

pub struct Translator {
    api_key: String,
    client: Client,
    api_base: String,
}

impl Translator {
    pub fn new(api_key: String) -> Self {
        let api_base = if api_key.trim_end().ends_with(":fx") {
            "https://api-free.deepl.com".to_string()
        } else {
            "https://api.deepl.com".to_string()
        };

        Self {
            api_key,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            api_base,
        }
    }

    /// Sanitize user input to prevent prompt injection
    fn sanitize_input(&self, text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(2000) // Limit length
            .collect::<String>()
            .replace("<", "&lt;")
            .replace(">", "&gt;")
    }

    fn map_language_code(&self, lang: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let normalized = lang.trim().to_lowercase();
        let code = match normalized.as_str() {
            "ja" | "japanese" | "jp" => "JA",
            "ko" | "korean" | "kr" => "KO",
            "en" | "english" | "en-us" | "en_us" => "EN-US",
            "en-gb" | "en_gb" => "EN-GB",
            _ => {
                return Err(format!("Unsupported language code: {}", lang).into());
            }
        };
        Ok(code.to_string())
    }

    /// Translate text using DeepL API
    pub async fn translate(
        &self,
        text: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let sanitized_text = self.sanitize_input(text);

        if sanitized_text.trim().is_empty() {
            return Ok(String::new());
        }

        let source_code = self.map_language_code(source_lang)?;
        let target_code = self.map_language_code(target_lang)?;
        let url = format!("{}/v2/translate", self.api_base);

        let mut last_error: Option<String> = None;
        let max_attempts = 3;

        for attempt in 1..=max_attempts {
            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
                .form(&[
                    ("text", sanitized_text.as_str()),
                    ("source_lang", source_code.as_str()),
                    ("target_lang", target_code.as_str()),
                ])
                .send()
                .await;

            let response = match response {
                Ok(resp) => resp,
                Err(e) => {
                    last_error = Some(format!("DeepL request failed: {}", e));
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(200 * attempt as u64)).await;
                        continue;
                    }
                    return Err(last_error.unwrap_or_else(|| "DeepL request failed".to_string()).into());
                }
            };

            if response.status().is_success() {
                let deepl_response: DeepLResponse = response.json().await?;
                if let Some(translation) = deepl_response.translations.first() {
                    return Ok(translation.text.trim().to_string());
                }
                return Err("No translation returned from DeepL API".into());
            }

            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            let status_code = status.as_u16();

            let retryable = matches!(status_code, 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < max_attempts {
                last_error = Some(format!("DeepL API error: {} - {}", status, error_text));
                sleep(Duration::from_millis(200 * attempt as u64)).await;
                continue;
            }

            if status_code == 456 {
                return Err("DeepL API quota exceeded (456)".into());
            }

            return Err(format!("DeepL API error: {} - {}", status, error_text).into());
        }

        Err(last_error.unwrap_or_else(|| "DeepL API error".to_string()).into())
    }

    /// Detect language locally based on character analysis
    pub fn detect_language_local(text: &str) -> String {
        let mut hiragana_count = 0;
        let mut katakana_count = 0;
        let mut kanji_count = 0;
        
        for c in text.chars() {
            if ('\u{3040}'..='\u{309F}').contains(&c) {
                hiragana_count += 1;
            } else if ('\u{30A0}'..='\u{30FF}').contains(&c) {
                katakana_count += 1;
            } else if ('\u{4E00}'..='\u{9FFF}').contains(&c) {
                kanji_count += 1;
            }
        }
        
        let total_chars = text.chars().count();
        let japanese_chars = hiragana_count + katakana_count + kanji_count;
        
        if total_chars > 0 && japanese_chars * 10 > total_chars {
            "Japanese".to_string()
        } else {
            "English".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_japanese() {
        let text = "こんにちは世界";
        assert_eq!(Translator::detect_language_local(text), "Japanese");
    }

    #[test]
    fn test_detect_language_english() {
        let text = "Hello World";
        assert_eq!(Translator::detect_language_local(text), "English");
    }

    #[test]
    fn test_sanitize_input() {
        let translator = Translator::new("test:fx".to_string());
        
        // Test HTML escaping
        assert_eq!(translator.sanitize_input("<script>"), "&lt;script&gt;");
        
        // Test length limit
        let long_text = "a".repeat(3000);
        assert_eq!(translator.sanitize_input(&long_text).len(), 2000);
        
        // Test control character removal
        assert_eq!(translator.sanitize_input("hello\x00world"), "helloworld");
    }

    #[test]
    fn test_language_mapping() {
        let translator = Translator::new("test:fx".to_string());
        assert_eq!(translator.map_language_code("ja").unwrap(), "JA");
        assert_eq!(translator.map_language_code("ko").unwrap(), "KO");
        assert_eq!(translator.map_language_code("en").unwrap(), "EN-US");
        assert_eq!(translator.map_language_code("en-us").unwrap(), "EN-US");
        assert_eq!(translator.map_language_code("en-gb").unwrap(), "EN-GB");
    }
}
