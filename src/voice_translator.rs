use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use twilight_model::id::Id;
use chrono::Local;
use songbird::events::{EventContext, EventHandler as SongbirdEventHandler};

pub type SpeakerId = Id<twilight_model::id::marker::UserMarker>;

#[derive(Debug, Clone)]
pub struct TranslationPair {
    pub source_lang: String,
    pub target_lang: String,
}

impl TranslationPair {
    pub fn new(source: &str, target: &str) -> Self {
        Self {
            source_lang: source.to_string(),
            target_lang: target.to_string(),
        }
    }
}

/// Buffer for accumulating audio samples for translation
#[derive(Clone)]
pub struct TranslationBuffer {
    pub user_id: SpeakerId,
    pub samples: Vec<i16>,
    pub last_activity: chrono::DateTime<Local>,
    pub is_speaking: bool,
}

impl TranslationBuffer {
    pub fn new(user_id: SpeakerId) -> Self {
        Self {
            user_id,
            samples: Vec::new(),
            last_activity: Local::now(),
            is_speaking: false,
        }
    }

    pub fn add_samples(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
        self.last_activity = Local::now();
        self.is_speaking = true;
    }

    pub fn mark_silence(&mut self) {
        self.is_speaking = false;
    }

    /// Check if buffer should be flushed (silence detected for specified duration)
    pub fn should_flush(&self, silence_duration_ms: u64) -> bool {
        if self.samples.is_empty() {
            return false;
        }
        
        let elapsed = Local::now().signed_duration_since(self.last_activity);
        elapsed.num_milliseconds() > silence_duration_ms as i64
    }

    /// Check if minimum speech duration is met
    pub fn has_minimum_duration(&self, min_samples: usize) -> bool {
        self.samples.len() >= min_samples
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.is_speaking = false;
    }
}

/// Manages real-time voice translation session
#[derive(Clone)]
pub struct TranslationSession {
    pub guild_id: Id<twilight_model::id::marker::GuildMarker>,
    pub channel_id: Id<twilight_model::id::marker::ChannelMarker>,
    pub translation_pair: TranslationPair,
    pub start_time: chrono::DateTime<Local>,
    /// Buffers for each speaker (SSRC -> TranslationBuffer)
    pub speaker_buffers: Arc<RwLock<HashMap<u32, TranslationBuffer>>>,
    /// SSRC to User ID mapping
    pub ssrc_to_user: Arc<RwLock<HashMap<u32, SpeakerId>>>,
}

impl TranslationSession {
    pub fn new(
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        channel_id: Id<twilight_model::id::marker::ChannelMarker>,
        translation_pair: TranslationPair,
    ) -> Self {
        Self {
            guild_id,
            channel_id,
            translation_pair,
            start_time: Local::now(),
            speaker_buffers: Arc::new(RwLock::new(HashMap::new())),
            ssrc_to_user: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add audio samples from a speaker
    pub async fn add_audio(&self, ssrc: u32, user_id: SpeakerId, samples: &[i16]) {
        // Update SSRC mapping
        {
            let mut ssrc_map = self.ssrc_to_user.write().await;
            ssrc_map.insert(ssrc, user_id);
        }

        // Add to buffer
        let mut buffers = self.speaker_buffers.write().await;
        let buffer = buffers.entry(ssrc).or_insert_with(|| TranslationBuffer::new(user_id));
        buffer.add_samples(samples);
    }

    /// Mark silence for a speaker (called when VAD detects silence)
    pub async fn mark_silence(&self, ssrc: u32) {
        let mut buffers = self.speaker_buffers.write().await;
        if let Some(buffer) = buffers.get_mut(&ssrc) {
            buffer.mark_silence();
        }
    }

    /// Get buffers that are ready for translation (silence detected and minimum duration met)
    pub async fn get_ready_buffers(&self) -> Vec<(SpeakerId, Vec<i16>)> {
        let mut ready = Vec::new();
        let mut buffers = self.speaker_buffers.write().await;
        let ssrc_map = self.ssrc_to_user.read().await;
        
        // Silence duration: 1.5 seconds (1500ms)
        // Minimum duration: 0.5 seconds at 48kHz = 24000 samples
        const SILENCE_MS: u64 = 1500;
        const MIN_SAMPLES: usize = 24000;

        for (ssrc, buffer) in buffers.iter_mut() {
            if buffer.should_flush(SILENCE_MS) && buffer.has_minimum_duration(MIN_SAMPLES) {
                if let Some(&user_id) = ssrc_map.get(ssrc) {
                    ready.push((user_id, buffer.samples.clone()));
                    buffer.clear();
                }
            }
        }

        ready
    }
}

/// Manages active translation sessions
#[derive(Clone)]
pub struct TranslationManager {
    active_sessions: Arc<RwLock<HashMap<Id<twilight_model::id::marker::GuildMarker>, TranslationSession>>>,
}

impl TranslationManager {
    pub fn new() -> Self {
        Self {
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start_translation(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        channel_id: Id<twilight_model::id::marker::ChannelMarker>,
        translation_pair: TranslationPair,
    ) -> TranslationSession {
        let session = TranslationSession::new(guild_id, channel_id, translation_pair);
        let mut sessions = self.active_sessions.write().await;
        sessions.insert(guild_id, session.clone());
        println!("[INFO] Started translation session for guild {}", guild_id);
        session
    }

    pub async fn stop_translation(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
    ) -> Option<TranslationSession> {
        let mut sessions = self.active_sessions.write().await;
        let session = sessions.remove(&guild_id);
        if session.is_some() {
            println!("[INFO] Stopped translation session for guild {}", guild_id);
        }
        session
    }

    pub async fn is_translating(&self, guild_id: Id<twilight_model::id::marker::GuildMarker>) -> bool {
        let sessions = self.active_sessions.read().await;
        sessions.contains_key(&guild_id)
    }

    pub async fn add_audio_to_session(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        ssrc: u32,
        user_id: SpeakerId,
        samples: &[i16],
    ) {
        let sessions = self.active_sessions.read().await;
        if let Some(session) = sessions.get(&guild_id) {
            session.add_audio(ssrc, user_id, samples).await;
        }
    }

    pub async fn get_ready_translations(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
    ) -> Vec<(SpeakerId, Vec<i16>)> {
        let sessions = self.active_sessions.read().await;
        if let Some(session) = sessions.get(&guild_id) {
            session.get_ready_buffers().await
        } else {
            Vec::new()
        }
    }
}

/// Event handler for voice translation
#[derive(Clone)]
pub struct VoiceTranslateHandler {
    pub translation_manager: Arc<TranslationManager>,
    pub guild_id: Id<twilight_model::id::marker::GuildMarker>,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, SpeakerId>>>,
}

impl VoiceTranslateHandler {
    pub fn new(
        translation_manager: Arc<TranslationManager>,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
    ) -> Self {
        Self {
            translation_manager,
            guild_id,
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl SongbirdEventHandler for VoiceTranslateHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<songbird::Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user_id) = speaking.user_id {
                    let ssrc = speaking.ssrc;
                    let user_id = Id::new(user_id.0);
                    
                    println!("[DEBUG] Translation SpeakingStateUpdate: SSRC {} -> User {}", ssrc, user_id);
                    
                    let mut ssrc_map = self.ssrc_to_user.lock().await;
                    ssrc_map.insert(ssrc, user_id);
                }
            }
            EventContext::VoiceTick(tick) => {
                for (ssrc, voice_data) in tick.speaking.iter() {
                    if let Some(ref audio) = voice_data.decoded_voice {
                        let samples: Vec<i16> = audio.clone();
                        
                        if !samples.is_empty() {
                            let ssrc_map = self.ssrc_to_user.lock().await;
                            if let Some(&user_id) = ssrc_map.get(ssrc) {
                                drop(ssrc_map);
                                self.translation_manager.add_audio_to_session(
                                    self.guild_id,
                                    *ssrc,
                                    user_id,
                                    &samples,
                                ).await;
                            }
                        }
                    } else {
                        // No audio data - mark as silence for VAD
                        self.translation_manager.add_audio_to_session(
                            self.guild_id,
                            *ssrc,
                            Id::new(0), // Placeholder, won't be used
                            &[],
                        ).await;
                    }
                }
            }
            _ => {}
        }
        
        None
    }
}
