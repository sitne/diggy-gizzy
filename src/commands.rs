use std::sync::Arc;
use twilight_model::id::Id;
use twilight_http::Client as HttpClient;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseType};

use crate::voice_recorder::RecordingManager;
use crate::transcriber::{Transcriber, transcribe_wav_file};
use crate::summarizer::Summarizer;

pub struct RecordingCommands {
    pub recording_manager: Arc<RecordingManager>,
    pub transcriber: Arc<Transcriber>,
    pub summarizer: Arc<Summarizer>,
}

impl RecordingCommands {
    pub fn new(
        recording_manager: Arc<RecordingManager>,
        transcriber: Arc<Transcriber>,
        summarizer: Arc<Summarizer>,
    ) -> Self {
        Self {
            recording_manager,
            transcriber,
            summarizer,
        }
    }

    pub async fn handle_record_start(
        &self,
        interaction_id: Id<twilight_model::id::marker::InteractionMarker>,
        token: String,
        http: Arc<HttpClient>,
        application_id: Id<twilight_model::id::marker::ApplicationMarker>,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        channel_id: Id<twilight_model::id::marker::ChannelMarker>,
        _user_id: Id<twilight_model::id::marker::UserMarker>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("[DEBUG] handle_record_start called for guild: {}, channel: {}", guild_id, channel_id);
        
        // Check if recording already active
        let has_active_session = {
            let has = self.recording_manager.is_recording(guild_id).await;
            println!("[DEBUG] Has active session: {}", has);
            has
        };

        if has_active_session {
            println!("[DEBUG] Already recording, sending error response");
            let response = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(twilight_model::http::interaction::InteractionResponseData {
                    content: Some("❌ Already recording in this server. Use `/record stop` first.".to_string()),
                    ..Default::default()
                }),
            };

            if let Err(e) = http
                .interaction(application_id)
                .create_response(interaction_id, &token, &response)
                .await
            {
                eprintln!("[ERROR] Failed to send response: {}", e);
            }
            return Ok(());
        }

        // Start recording session
        println!("[DEBUG] Starting recording session");
        let _session = self.recording_manager.start_recording(guild_id, channel_id).await;
        println!("[DEBUG] Recording session started");

        // Send success response
        let response = InteractionResponse {
            kind: InteractionResponseType::ChannelMessageWithSource,
            data: Some(twilight_model::http::interaction::InteractionResponseData {
                content: Some("🔴 **Recording started!**\n\nThe bot is now ready to record. However, please note:\n• The bot needs to be in a voice channel to capture audio\n• Make sure the bot has permission to join voice channels\n• Use `/record_stop` to stop recording and generate meeting minutes".to_string()),
                ..Default::default()
            }),
        };

        if let Err(e) = http
            .interaction(application_id)
            .create_response(interaction_id, &token, &response)
            .await
        {
            eprintln!("[ERROR] Failed to send response: {}", e);
        }

        println!("[DEBUG] handle_record_start completed");
        Ok(())
    }

    pub async fn handle_record_stop(
        &self,
        interaction_id: Id<twilight_model::id::marker::InteractionMarker>,
        token: String,
        http: Arc<HttpClient>,
        application_id: Id<twilight_model::id::marker::ApplicationMarker>,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        text_channel_id: Option<Id<twilight_model::id::marker::ChannelMarker>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("[DEBUG] handle_record_stop called for guild: {}", guild_id);

        let response_content = match self.recording_manager.stop_recording(guild_id).await {
            Ok(Some(session)) => {
                let speaker_files = session.finalize("./recordings").await.unwrap_or_default();
                if !speaker_files.is_empty() {
                    println!("[DEBUG] Found {} speaker files to process", speaker_files.len());
                    
                    // Send initial response
                    let initial_response = InteractionResponse {
                        kind: InteractionResponseType::ChannelMessageWithSource,
                        data: Some(twilight_model::http::interaction::InteractionResponseData {
                            content: Some("🛑 **Recording stopped!**\nProcessing audio files and generating meeting minutes...".to_string()),
                            ..Default::default()
                        }),
                    };

                    if let Err(e) = http
                        .interaction(application_id)
                        .create_response(interaction_id, &token, &initial_response)
                        .await
                    {
                        eprintln!("[ERROR] Failed to send initial response: {}", e);
                    }

                    let mut full_transcript = String::new();
                    let mut transcription_errors = Vec::new();

                    for file_path in &speaker_files {
                        println!("[DEBUG] Transcribing file: {}", file_path);
                        match transcribe_wav_file(&self.transcriber, file_path).await {
                            Ok(transcription) => {
                                if !transcription.is_empty() {
                                    full_transcript.push_str(&format!("{}\n\n", transcription));
                                }
                            }
                            Err(e) => {
                                eprintln!("[ERROR] Failed to transcribe file {}: {}", file_path, e);
                                transcription_errors.push(format!("File {}: {}", file_path, e));
                            }
                        }

                        if let Err(e) = tokio::fs::remove_file(file_path).await {
                            eprintln!("[WARN] Failed to remove temporary file {}: {}", file_path, e);
                        }
                    }

                    if full_transcript.is_empty() {
                        "⚠️ **No audio detected** or transcription failed. Meeting minutes cannot be generated.\n\nNote: The recording infrastructure is set up, but audio capture requires the bot to be connected to a voice channel with proper permissions.".to_string()
                    } else {
                        println!("[DEBUG] Summarizing meeting with {} chars of transcript", full_transcript.len());
                        match self.summarizer.summarize_meeting(&full_transcript).await {
                            Ok(meeting_minutes) => {
                                let result = format!(
                                    "✅ **Meeting Minutes Generated**\n\n{}",
                                    meeting_minutes
                                );

                                if let Some(channel_id) = text_channel_id {
                                    let _ = http
                                        .create_message(channel_id)
                                        .content(&result)
                                        .await;
                                }

                                result
                            }
                            Err(e) => {
                                eprintln!("[ERROR] Failed to summarize meeting: {}", e);
                                format!(
                                    "⚠️ **Transcription completed but summarization failed**\n\n**Raw Transcription:**\n```\n{}\n```\n\nError: {}",
                                    full_transcript.chars().take(1900).collect::<String>(),
                                    e
                                )
                            }
                        }
                    }
                } else {
                    println!("[DEBUG] No speaker files found");
                    "⚠️ **No audio detected**. The recording session was stopped but no audio data was captured.\n\nNote: Make sure the bot is in a voice channel with users speaking.".to_string()
                }
            }
            _ => {
                println!("[DEBUG] No active recording found");
                "❌ No active recording found in this server. Use `/record start` first.".to_string()
            }
        };

        // Only send response if we haven't already (i.e., if no files were found)
        if !response_content.contains("Recording stopped!") {
            let response = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(twilight_model::http::interaction::InteractionResponseData {
                    content: Some(response_content),
                    ..Default::default()
                }),
            };

            if let Err(e) = http
                .interaction(application_id)
                .create_response(interaction_id, &token, &response)
                .await
            {
                eprintln!("[ERROR] Failed to send response: {}", e);
            }
        }

        println!("[DEBUG] handle_record_stop completed");
        Ok(())
    }
}
