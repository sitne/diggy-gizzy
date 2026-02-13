use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use twilight_model::id::Id;
use hound::{WavSpec, WavWriter};
use chrono::Local;
use songbird::events::{EventContext, EventHandler as SongbirdEventHandler};

pub type SpeakerId = Id<twilight_model::id::marker::UserMarker>;

#[derive(Clone)]
pub struct RecordingSession {
    pub guild_id: Id<twilight_model::id::marker::GuildMarker>,
    pub channel_id: Id<twilight_model::id::marker::ChannelMarker>,
    pub start_time: chrono::DateTime<Local>,
    pub speaker_buffers: Arc<RwLock<HashMap<SpeakerId, Vec<i16>>>>,
    output_dir: String,
}

impl RecordingSession {
    pub fn new(
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        channel_id: Id<twilight_model::id::marker::ChannelMarker>,
        output_dir: &str,
    ) -> Self {
        std::fs::create_dir_all(output_dir).ok();
        Self {
            guild_id,
            channel_id,
            start_time: Local::now(),
            speaker_buffers: Arc::new(RwLock::new(HashMap::new())),
            output_dir: output_dir.to_string(),
        }
    }

    pub async fn add_audio(&self, speaker_id: SpeakerId, samples: &[i16]) {
        // Store in memory buffer (for final WAV file)
        let mut buffers = self.speaker_buffers.write().await;
        let buffer = buffers.entry(speaker_id).or_insert_with(Vec::new);
        buffer.extend_from_slice(samples);
    }

    pub async fn finalize(&self, output_dir: &str) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let mut output_files = Vec::new();
        let buffers = self.speaker_buffers.read().await;

        for (speaker_id, samples) in buffers.iter() {
            if samples.is_empty() {
                continue;
            }
            
            let filename = format!(
                "{}/{}_{}_{}.wav",
                output_dir,
                self.guild_id,
                speaker_id,
                self.start_time.format("%Y%m%d_%H%M%S")
            );

            let spec = WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            let mut writer = WavWriter::create(&filename, spec)?;
            for &sample in samples {
                writer.write_sample(sample)?;
            }
            writer.finalize()?;
            output_files.push(filename);
        }

        if !output_files.is_empty() {
            println!("[INFO] Saved {} audio files", output_files.len());
        }

        Ok(output_files)
    }
}

#[derive(Clone)]
pub struct RecordingManager {
    output_dir: String,
    active_sessions: Arc<RwLock<HashMap<Id<twilight_model::id::marker::GuildMarker>, RecordingSession>>>,
}

impl RecordingManager {
    pub fn new(output_dir: String) -> Self {
        std::fs::create_dir_all(&output_dir).ok();
        Self {
            output_dir,
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start_recording(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        channel_id: Id<twilight_model::id::marker::ChannelMarker>,
    ) -> RecordingSession {
        let session = RecordingSession::new(guild_id, channel_id, &self.output_dir);
        let mut sessions = self.active_sessions.write().await;
        sessions.insert(guild_id, session.clone());
        println!("[INFO] Started recording for guild {}", guild_id);
        session
    }

    pub async fn stop_recording(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
    ) -> Result<Option<RecordingSession>, Box<dyn std::error::Error + Send + Sync>> {
        let mut sessions = self.active_sessions.write().await;
        let session = sessions.remove(&guild_id);
        if let Some(ref s) = session {
            println!("[INFO] Stopped recording for guild {}", guild_id);
        }
        Ok(session)
    }

    pub async fn add_audio_to_session(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        speaker_id: SpeakerId,
        samples: &[i16],
    ) {
        let sessions = self.active_sessions.read().await;
        if let Some(session) = sessions.get(&guild_id) {
            session.add_audio(speaker_id, samples).await;
        }
    }
    
    pub async fn is_recording(&self, guild_id: Id<twilight_model::id::marker::GuildMarker>) -> bool {
        let sessions = self.active_sessions.read().await;
        sessions.contains_key(&guild_id)
    }
    
    pub async fn flush_audio_buffers(
        &self,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        handler: &VoiceReceiveHandler,
    ) {
        let mut buffers = handler.audio_buffers.lock().await;
        let ssrc_map = handler.ssrc_to_user.lock().await;
        
        for (ssrc, buffer) in buffers.drain() {
            if !buffer.is_empty() {
                // Only process if we have a valid user mapping (not SSRC fallback)
                if let Some(&user_id) = ssrc_map.get(&ssrc) {
                    let sessions = self.active_sessions.read().await;
                    if let Some(session) = sessions.get(&guild_id) {
                        session.add_audio(user_id, &buffer).await;
                    }
                } else {
                    println!("[WARN] Skipping audio buffer for SSRC {} - no user mapping found", ssrc);
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct VoiceReceiveHandler {
    pub recording_manager: Arc<RecordingManager>,
    pub guild_id: Id<twilight_model::id::marker::GuildMarker>,
    pub audio_buffers: Arc<Mutex<HashMap<u32, Vec<i16>>>>,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, SpeakerId>>>,
}

impl VoiceReceiveHandler {
    pub fn new(
        recording_manager: Arc<RecordingManager>,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
    ) -> Self {
        Self {
            recording_manager,
            guild_id,
            audio_buffers: Arc::new(Mutex::new(HashMap::new())),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl SongbirdEventHandler for VoiceReceiveHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<songbird::Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user_id) = speaking.user_id {
                    let ssrc = speaking.ssrc;
                    let user_id = Id::new(user_id.0);
                    
                    println!("[DEBUG] SpeakingStateUpdate: SSRC {} -> User {}", ssrc, user_id);
                    
                    let mut ssrc_map = self.ssrc_to_user.lock().await;
                    ssrc_map.insert(ssrc, user_id);
                    println!("[DEBUG] SSRC map size: {}", ssrc_map.len());
                } else {
                    println!("[DEBUG] SpeakingStateUpdate: user_id is None for SSRC {}", speaking.ssrc);
                }
            }
            EventContext::VoiceTick(tick) => {
                for (ssrc, voice_data) in tick.speaking.iter() {
                    if let Some(ref audio) = voice_data.decoded_voice {
                        let samples: Vec<i16> = audio.clone();
                        
                        if !samples.is_empty() {
                            let ssrc_map = self.ssrc_to_user.lock().await;
                            // Only process if we have a valid user mapping
                            if let Some(&user_id) = ssrc_map.get(ssrc) {
                                drop(ssrc_map);
                                self.recording_manager.add_audio_to_session(
                                    self.guild_id,
                                    user_id,
                                    &samples,
                                ).await;
                            } else {
                                println!("[WARN] VoiceTick: No user mapping for SSRC {}, skipping audio", ssrc);
                            }
                        }
                    }
                }
            }
            EventContext::ClientDisconnect(disconnect) => {
                let user_id = disconnect.user_id;
            }
            _ => {}
        }
        
        None
    }
}
