use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use twilight_model::id::Id;
use twilight_model::id::marker::UserMarker;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLanguageSetting {
    pub source_lang: String,  // 話す言語 (ja, ko, en)
    pub target_lang: String,  // 翻訳先言語 (ja, ko, en)
}

impl UserLanguageSetting {
    pub fn new(source: &str, target: &str) -> Self {
        Self {
            source_lang: source.to_string(),
            target_lang: target.to_string(),
        }
    }

    pub fn to_full_name(&self, lang: &str) -> String {
        match lang {
            "ja" => "Japanese",
            "ko" => "Korean",
            "en" => "English",
            _ => lang,
        }.to_string()
    }

    pub fn get_source_full(&self) -> String {
        self.to_full_name(&self.source_lang)
    }

    pub fn get_target_full(&self) -> String {
        self.to_full_name(&self.target_lang)
    }
}

pub struct UserSettingsManager {
    settings: Arc<RwLock<HashMap<Id<UserMarker>, UserLanguageSetting>>>,
    file_path: String,
}

impl UserSettingsManager {
    pub fn new(file_path: &str) -> Self {
        let settings = Self::load_from_file(file_path);
        Self {
            settings: Arc::new(RwLock::new(settings)),
            file_path: file_path.to_string(),
        }
    }

    fn load_from_file(path: &str) -> HashMap<Id<UserMarker>, UserLanguageSetting> {
        if !Path::new(path).exists() {
            return HashMap::new();
        }

        match fs::read_to_string(path) {
            Ok(content) => {
                serde_json::from_str(&content).unwrap_or_default()
            }
            Err(_) => HashMap::new(),
        }
    }

    async fn save_to_file(&self) {
        let settings = self.settings.read().await;
        if let Ok(json) = serde_json::to_string_pretty(&*settings) {
            let _ = fs::write(&self.file_path, json);
        }
    }

    pub async fn set_user_language(
        &self,
        user_id: Id<UserMarker>,
        source_lang: &str,
        target_lang: &str,
    ) {
        let setting = UserLanguageSetting::new(source_lang, target_lang);
        {
            let mut settings = self.settings.write().await;
            settings.insert(user_id, setting);
        }
        self.save_to_file().await;
    }

    pub async fn get_user_setting(&self, user_id: Id<UserMarker>) -> Option<UserLanguageSetting> {
        let settings = self.settings.read().await;
        settings.get(&user_id).cloned()
    }

    pub async fn remove_user_setting(&self, user_id: Id<UserMarker>) {
        {
            let mut settings = self.settings.write().await;
            settings.remove(&user_id);
        }
        self.save_to_file().await;
    }

    pub async fn list_all_settings(&self) -> Vec<(Id<UserMarker>, UserLanguageSetting)> {
        let settings = self.settings.read().await;
        settings.iter().map(|(k, v)| (*k, v.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_language_setting() {
        let setting = UserLanguageSetting::new("ja", "ko");
        assert_eq!(setting.source_lang, "ja");
        assert_eq!(setting.target_lang, "ko");
        assert_eq!(setting.get_source_full(), "Japanese");
        assert_eq!(setting.get_target_full(), "Korean");
    }
}
