use std::{env, error::Error, num::NonZeroU64, sync::Arc, collections::HashMap};
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client as HttpClient;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    application::interaction::{Interaction, InteractionData, InteractionType},
    gateway::payload::incoming::ReactionAdd,
    gateway::payload::incoming::ReactionRemove,
    http::interaction::{InteractionResponse, InteractionResponseType},
    id::Id,
};
use tokio::sync::Mutex;
use songbird::Songbird;
use songbird::events::{Event as SongbirdEvent, CoreEvent};
use songbird::shards::TwilightMap;
use songbird::driver::{DecodeMode, Channels, SampleRate};

mod voice_recorder;
mod transcriber;
mod summarizer;
mod commands;

use voice_recorder::{RecordingManager, VoiceReceiveHandler};
use transcriber::{Transcriber, transcribe_wav_file};
use summarizer::Summarizer;
use commands::RecordingCommands;

#[derive(CommandModel, CreateCommand)]
#[command(name = "record", desc = "Join voice channel and start recording control")]
struct RecordCommand;



struct BotState {
    http: Arc<HttpClient>,
    application_id: Id<twilight_model::id::marker::ApplicationMarker>,
    http_client: ReqwestClient,
    recording_commands: RecordingCommands,
    user_voice_states: Arc<Mutex<HashMap<Id<twilight_model::id::marker::UserMarker>, Id<twilight_model::id::marker::ChannelMarker>>>>,
    songbird: Arc<Songbird>,
    voice_handlers: Arc<Mutex<HashMap<Id<twilight_model::id::marker::GuildMarker>, voice_recorder::VoiceReceiveHandler>>>,
    // Reaction control: (message_id, channel_id, guild_id, user_id) -> is_recording
    reaction_controls: Arc<Mutex<HashMap<(Id<twilight_model::id::marker::MessageMarker>, Id<twilight_model::id::marker::ChannelMarker>, Id<twilight_model::id::marker::GuildMarker>, Id<twilight_model::id::marker::UserMarker>), bool>>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let token = env::var("DISCORD_TOKEN")
        .map_err(|_| "DISCORD_TOKEN not set")?;

    let application_id = env::var("DISCORD_APPLICATION_ID")
        .map_err(|_| "DISCORD_APPLICATION_ID not set")?
        .parse::<u64>()
        .map_err(|_| "Invalid DISCORD_APPLICATION_ID")?;

    let zai_api_key = env::var("ZAI_API_KEY")
        .unwrap_or_default();

    let whisper_model_path = env::var("WHISPER_MODEL_PATH")
        .unwrap_or_else(|_| "./models/ggml-base.bin".to_string());

    let http_client = ReqwestClient::new();
    let intents = Intents::GUILD_VOICE_STATES | Intents::GUILDS | Intents::GUILD_MEMBERS | Intents::GUILD_MESSAGE_REACTIONS | Intents::GUILD_MESSAGES;
    let mut shard = Shard::new(ShardId::ONE, token.clone(), intents);
    let http = Arc::new(HttpClient::new(token));
    let application_id = Id::new(application_id);

    // Get bot user ID for songbird
    let bot_user_id = http.current_user().await?.model().await?.id;

    // Initialize Songbird with TwilightMap
    let shard_sender = shard.sender();
    let mut map = HashMap::new();
    map.insert(ShardId::ONE.number(), shard_sender);
    let twilight_map = TwilightMap::new(map);
    let songbird = Songbird::twilight(Arc::new(twilight_map), bot_user_id);
    
    // Configure Songbird to decode received audio as mono 48kHz
    songbird.set_config(
        songbird::Config::default()
            .decode_mode(DecodeMode::Decode)
            .decode_channels(Channels::Mono)
            .decode_sample_rate(SampleRate::Hz48000)
            .use_softclip(true),
    );

    let recording_manager = Arc::new(RecordingManager::new("./recordings".to_string()));
    let transcriber = Arc::new(Transcriber::new(&whisper_model_path)?);
    let summarizer = Arc::new(Summarizer::new(zai_api_key));

    let recording_commands = RecordingCommands::new(
        recording_manager,
        transcriber,
        summarizer,
    );

    // Register global commands
    println!("[INFO] Registering global commands...");
    let interaction_client = http.interaction(application_id);
    
    // First, clear all existing global commands to avoid conflicts with old commands
    println!("[INFO] Clearing existing global commands...");
    match interaction_client.set_global_commands(&[]).await {
        Ok(_) => println!("[INFO] Global commands cleared successfully"),
        Err(e) => eprintln!("[WARN] Failed to clear global commands: {}", e),
    }
    
    // Register only record command
    match interaction_client
        .create_global_command()
        .chat_input("record", "Join voice channel and start recording control")
        .await
    {
        Ok(_) => println!("[INFO] Registered global command: record"),
        Err(e) => eprintln!("[ERROR] Failed to register command 'record': {}", e),
    }
    
    println!("[INFO] Global commands registration completed");
    
    // Note: Guild commands are automatically removed when the bot leaves a guild
    // or can be manually removed by kicking and re-inviting the bot to a guild

    let bot_state = Arc::new(BotState {
        http: http.clone(),
        application_id,
        http_client,
        recording_commands,
        user_voice_states: Arc::new(Mutex::new(HashMap::new())),
        songbird: Arc::new(songbird),
        voice_handlers: Arc::new(Mutex::new(HashMap::new())),
        reaction_controls: Arc::new(Mutex::new(HashMap::new())),
    });

    println!("Bot is starting...");

    while let Some(item) = shard.next_event(EventTypeFlags::all()).await {
        let Ok(event) = item else {
            tracing::warn!(source = ?item.unwrap_err(), "error receiving event");
            continue;
        };

        let state = Arc::clone(&bot_state);
        tokio::spawn(async move {
            if let Err(e) = handle_event(event, state).await {
                eprintln!("Error handling event: {}", e);
            }
        });
    }

    Ok(())
}

// Helper function to extract user_id from WAV filename
// Format: {guild_id}_{user_id}_{timestamp}.wav
fn extract_user_id_from_filename(file_path: &str) -> Option<Id<twilight_model::id::marker::UserMarker>> {
    use std::path::Path;
    
    Path::new(file_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|name| {
            let parts: Vec<&str> = name.split('_').collect();
            if parts.len() >= 2 {
                parts[1].parse::<u64>().ok().map(Id::new)
            } else {
                None
            }
        })
}

async fn handle_event(
    event: Event,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        Event::InteractionCreate(interaction_create) => {
            let interaction = interaction_create.0;

            if interaction.kind == InteractionType::ApplicationCommand {
                handle_command(interaction, state).await?;
            }
        }
        Event::VoiceStateUpdate(voice_state_update) => {
            let voice_state = voice_state_update.0.clone();
            let user_id = voice_state.user_id;
            let guild_id = voice_state.guild_id;
            
            // Update songbird with voice state
            state.songbird.process(&Event::VoiceStateUpdate(voice_state_update)).await;
            
            if let Some(_guild_id) = guild_id {
                if let Some(channel_id) = voice_state.channel_id {
                    let mut voice_states = state.user_voice_states.lock().await;
                    voice_states.insert(user_id, channel_id);
                } else {
                    let mut voice_states = state.user_voice_states.lock().await;
                    voice_states.remove(&user_id);
                }
            }
        }
        Event::VoiceServerUpdate(voice_server_update) => {
            // Process voice server updates for songbird
            state.songbird.process(&Event::VoiceServerUpdate(voice_server_update)).await;
        }
        Event::ReactionAdd(reaction_add) => {
            handle_reaction_add(*reaction_add, state).await?;
        }
        Event::ReactionRemove(reaction_remove) => {
            handle_reaction_remove(*reaction_remove, state).await?;
        }
        _ => {}
    }

    Ok(())
}

async fn handle_reaction_add(
    reaction: ReactionAdd,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Check if this is a 🔴 reaction on a control message
    let emoji = &reaction.emoji;
    let message_id = reaction.message_id;
    let channel_id = reaction.channel_id;
    let guild_id = match reaction.guild_id {
        Some(id) => id,
        None => {
            eprintln!("[ERROR] Reaction add: No guild_id in reaction");
            return Ok(());
        }
    };
    let user_id = reaction.user_id;
    
    println!("[DEBUG] Reaction add: emoji={:?}, user_id={}, message_id={}, channel_id={}, guild_id={}", 
             emoji, user_id, message_id, channel_id, guild_id);
    
    // Only handle 🔴 emoji
    // EmojiReactionType is an enum with Unicode and Custom variants
    let is_target_emoji = matches!(emoji, twilight_model::channel::message::EmojiReactionType::Unicode { name } if name == "🔴");
    
    if !is_target_emoji {
        println!("[DEBUG] Reaction add: Emoji is not 🔴, ignoring");
        return Ok(());
    }
    
    // Check if this is a control message
    let key = (message_id, channel_id, guild_id, user_id);
    println!("[DEBUG] Reaction add: Looking up control key: {:?}", key);
    
    let mut controls = state.reaction_controls.lock().await;
    
    let control_entry = controls.get(&key);
    match control_entry {
        Some(is_recording) => {
            println!("[DEBUG] Reaction add: Found control entry, is_recording={}", is_recording);
            if !*is_recording {
                // Start recording
                println!("[INFO] Starting recording via reaction for user {} in guild {}", user_id, guild_id);
                
                // Get the user's voice channel
                let voice_states = state.user_voice_states.lock().await;
                println!("[DEBUG] Reaction add: User voice states count: {}", voice_states.len());
                println!("[DEBUG] Reaction add: Looking for user {} in voice states", user_id);
                
                if let Some(channel_id) = voice_states.get(&user_id).copied() {
                    println!("[DEBUG] Reaction add: Found user in voice channel {}", channel_id);
                    drop(voice_states);
                    
                    // Join voice channel
                    let channel_id_nz = match NonZeroU64::new(channel_id.get()) {
                        Some(id) => {
                            println!("[DEBUG] Reaction add: Created NonZeroU64: {}", id);
                            id
                        }
                        None => {
                            eprintln!("[ERROR] Failed to create NonZeroU64 from channel_id: {}", channel_id.get());
                            return Ok(());
                        }
                    };
                    
                    println!("[DEBUG] Reaction add: Attempting to join voice channel {} in guild {}", channel_id_nz, guild_id);
                    let call_result = state.songbird.join(guild_id, channel_id_nz).await;
                    
                    match call_result {
                        Ok(call) => {
                            println!("[INFO] Successfully joined voice channel {}", channel_id);
                            
                            // Add voice receive handler
                            let receive_handler = VoiceReceiveHandler::new(
                                state.recording_commands.recording_manager.clone(),
                                guild_id,
                            );
                            
                            let mut call_lock = call.lock().await;
                            call_lock.add_global_event(
                                SongbirdEvent::Core(CoreEvent::SpeakingStateUpdate),
                                receive_handler.clone(),
                            );
                            call_lock.add_global_event(
                                SongbirdEvent::Core(CoreEvent::VoiceTick),
                                receive_handler.clone(),
                            );
                            call_lock.add_global_event(
                                SongbirdEvent::Core(CoreEvent::ClientDisconnect),
                                receive_handler.clone(),
                            );
                            drop(call_lock);
                            
                            // Store the voice handler in state
                            state.voice_handlers.lock().await.insert(guild_id, receive_handler);
                            
                            // Start recording session
                            state.recording_commands.recording_manager.start_recording(guild_id, channel_id).await;
                            
                            // Update control state
                            controls.insert(key, true);
                            
                            // Send message to channel
                            match state.http.create_message(channel_id)
                                .content("🔴 **Recording started!**")
                                .await
                            {
                                Ok(_) => println!("[INFO] Successfully sent 'Recording started' message"),
                                Err(e) => eprintln!("[ERROR] Failed to send 'Recording started' message: {}", e),
                            }
                        }
                        Err(e) => {
                            eprintln!("[ERROR] Failed to join voice channel: {:?}", e);
                            // Notify user
                            let _ = state.http.create_message(channel_id)
                                .content(&format!("❌ Failed to join voice channel: {}", e))
                                .await;
                        }
                    }
                } else {
                    eprintln!("[ERROR] User {} not found in voice states. Available users: {:?}", 
                             user_id, voice_states.keys().collect::<Vec<_>>());
                    // Notify user
                    let _ = state.http.create_message(channel_id)
                        .content("❌ You must be in a voice channel to start recording!")
                        .await;
                }
            } else {
                println!("[DEBUG] Reaction add: Recording is already active, ignoring");
            }
        }
        None => {
            eprintln!("[ERROR] No control entry found for key: {:?}. Total registered controls: {}", 
                     key, controls.len());
            // Log all registered keys for debugging
            for registered_key in controls.keys() {
                println!("[DEBUG] Registered control: {:?}", registered_key);
            }
        }
    }
    
    Ok(())
}

async fn handle_reaction_remove(
    reaction: ReactionRemove,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Check if this is a 🔴 reaction on a control message
    let emoji = &reaction.emoji;
    let message_id = reaction.message_id;
    let channel_id = reaction.channel_id;
    let guild_id = reaction.guild_id.ok_or("No guild")?;
    let user_id = reaction.user_id;
    
    println!("[DEBUG] Reaction remove: emoji={:?}, user_id={}, message_id={}, channel_id={}, guild_id={}", 
             emoji, user_id, message_id, channel_id, guild_id);
    
    // Only handle 🔴 emoji
    // EmojiReactionType is an enum with Unicode and Custom variants
    let is_target_emoji = matches!(emoji, twilight_model::channel::message::EmojiReactionType::Unicode { name } if name == "🔴");
    
    if !is_target_emoji {
        return Ok(());
    }
    
    // Check if this is a control message
    let key = (message_id, channel_id, guild_id, user_id);
    let mut controls = state.reaction_controls.lock().await;
    
    if let Some(is_recording) = controls.get(&key) {
        if *is_recording {
            // Stop recording
            println!("[INFO] Stopping recording via reaction for user {} in guild {}", user_id, guild_id);
            
            // Update control state back to not recording (don't remove, so it can be restarted)
            controls.insert(key, false);
            drop(controls);
            
            // Leave voice channel
            let has_call = state.songbird.get(guild_id).is_some();
            
            if has_call {
                // Flush audio buffers
                if let Some(handler) = state.voice_handlers.lock().await.remove(&guild_id) {
                    state.recording_commands.recording_manager.flush_audio_buffers(guild_id, &handler).await;
                }
                
                if let Err(e) = state.songbird.leave(guild_id).await {
                    eprintln!("[ERROR] Failed to leave voice channel: {}", e);
                }
            }
            
            // Get the voice channel ID to send messages to the voice channel chat
            let voice_states = state.user_voice_states.lock().await;
            let voice_channel_id = voice_states.get(&user_id).copied();
            drop(voice_states);
            
            // Stop recording and process
            let session = state.recording_commands.recording_manager.stop_recording(guild_id).await?;
            
            if let Some(session) = session {
                let speaker_files = session.finalize("./recordings").await.unwrap_or_default();
                
                if !speaker_files.is_empty() {
                    // Cache for user info to avoid duplicate API calls
                    let mut user_cache: std::collections::HashMap<Id<twilight_model::id::marker::UserMarker>, String> = std::collections::HashMap::new();
                    
                    // Transcribe and summarize with speaker labels
                    let mut full_transcript = String::new();
                    let mut transcription_errors = Vec::new();
                    
                    for file_path in &speaker_files {
                        println!("[INFO] Transcribing file: {}", file_path);
                        
                        // Extract user_id from filename (format: {guild_id}_{user_id}_{timestamp}.wav)
                        let speaker_id = extract_user_id_from_filename(file_path);
                        
                        // Get or fetch speaker display name
                        let speaker_name = if let Some(id) = speaker_id {
                            if let Some(name) = user_cache.get(&id) {
                                name.clone()
                            } else {
                                // Fetch guild member info
                                let display_name = match state.http.guild_member(guild_id, id).await {
                                    Ok(response) => {
                                        if let Ok(member) = response.model().await {
                                            // Use nickname if available, otherwise global username
                                            member.nick.clone()
                                                .map(|n| format!("{} ({})", n, member.user.name))
                                                .unwrap_or_else(|| member.user.name.clone())
                                        } else {
                                            format!("User {}", id)
                                        }
                                    }
                                    Err(_) => format!("User {}", id),
                                };
                                user_cache.insert(id, display_name.clone());
                                display_name
                            }
                        } else {
                            "Unknown Speaker".to_string()
                        };
                        
                        match transcribe_wav_file(&state.recording_commands.transcriber, file_path).await {
                            Ok(transcription) => {
                                if !transcription.is_empty() {
                                    // Add speaker label to each line of transcription
                                    let labeled_text: String = transcription
                                        .lines()
                                        .map(|line| format!("**[{}]**: {}", speaker_name, line))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    full_transcript.push_str(&format!("{}\n\n", labeled_text));
                                }
                            }
                            Err(e) => {
                                eprintln!("[ERROR] Failed to transcribe file {}: {}", file_path, e);
                                transcription_errors.push(format!("File {}: {}", file_path, e));
                            }
                        }
                        
                        // Delete the WAV file after transcription to save disk space
                        if let Err(e) = tokio::fs::remove_file(file_path).await {
                            eprintln!("[WARN] Failed to remove temporary file {}: {}", file_path, e);
                        } else {
                            println!("[INFO] Deleted temporary file: {}", file_path);
                        }
                    }
                    
                    // Send messages to the voice channel chat if available
                    let target_channel_id = voice_channel_id.unwrap_or(channel_id);
                    
                    if full_transcript.is_empty() {
                        let _ = state.http.create_message(target_channel_id)
                            .content("⚠️ **No audio detected** or transcription failed. Meeting minutes cannot be generated.")
                            .await;
                    } else {
                        println!("[INFO] Summarizing meeting with {} chars of transcript", full_transcript.len());
                        match state.recording_commands.summarizer.summarize_meeting(&full_transcript).await {
                            Ok(meeting_minutes) => {
                                // Send full transcript first
                                let transcript_msg = format!(
                                    "📝 **Full Transcription**\n```\n{}\n```",
                                    full_transcript.chars().take(1950).collect::<String>()
                                );
                                match state.http.create_message(target_channel_id)
                                    .content(&transcript_msg)
                                    .await {
                                    Ok(_) => println!("[INFO] Sent full transcript to voice channel {}", target_channel_id),
                                    Err(e) => eprintln!("[ERROR] Failed to send transcript: {}", e),
                                }
                                
                                // Then send meeting minutes
                                let result = format!(
                                    "✅ **Meeting Minutes Generated**\n\n{}",
                                    meeting_minutes
                                );
                                match state.http.create_message(target_channel_id)
                                    .content(&result)
                                    .await {
                                    Ok(_) => println!("[INFO] Sent meeting minutes to voice channel {}", target_channel_id),
                                    Err(e) => eprintln!("[ERROR] Failed to send meeting minutes: {}", e),
                                }
                            }
                            Err(e) => {
                                eprintln!("[ERROR] Failed to summarize meeting: {}", e);
                                let result = format!(
                                    "⚠️ **Transcription completed but summarization failed**\n\n**Raw Transcription:**\n```\n{}\n```\n\nError: {}",
                                    full_transcript.chars().take(1900).collect::<String>(),
                                    e
                                );
                                let _ = state.http.create_message(target_channel_id)
                                    .content(&result)
                                    .await;
                            }
                        }
                    }
                } else {
                    let _ = state.http.create_message(channel_id)
                        .content("❌ No audio data recorded")
                        .await;
                }
            }
        }
    }
    
    Ok(())
}

async fn handle_command(
    interaction: Interaction,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(InteractionData::ApplicationCommand(command_data)) = interaction.data {
        let interaction_id = interaction.id;
        let token = interaction.token.clone();
        let guild_id = interaction.guild_id;
        let _channel_id = interaction.channel_id;

        match command_data.name.as_str() {
            "record" => {
                if let Some(guild_id) = guild_id {
                    let user_voice_states = state.user_voice_states.lock().await;
                    let user_id = interaction
                        .user
                        .map(|u| u.id)
                        .or_else(|| interaction.member.as_ref().and_then(|m| m.user.as_ref().map(|u| u.id)));
                    let channel_id = interaction.channel_id;

                    if let (Some(user_id), Some(channel_id)) = (user_id, channel_id) {
                        // Send control message with 🔴 reaction
                        let control_message_response = state.http.create_message(channel_id)
                            .content("🔴 **Recording Control**\n\nPress 🔴 to start recording\nPress 🔴 again to stop and generate meeting minutes")
                            .await?;
                        
                        // Get the message model to access the id
                        let control_message = control_message_response.model().await?;
                        
                        // Add 🔴 reaction to the message using RequestReactionType
                        use twilight_http::request::channel::reaction::RequestReactionType;
                        state.http.create_reaction(channel_id, control_message.id, &RequestReactionType::Unicode { name: "🔴" }).await?;
                        
                        // Register this as a control message
                        let key = (control_message.id, channel_id, guild_id, user_id);
                        state.reaction_controls.lock().await.insert(key, false);
                        
                        // Send success response
                        let response = InteractionResponse {
                            kind: InteractionResponseType::ChannelMessageWithSource,
                            data: Some(twilight_model::http::interaction::InteractionResponseData {
                                content: Some("✅ **Recording control message created!**\n\nClick the 🔴 reaction above to start/stop recording.".to_string()),
                                ..Default::default()
                            }),
                        };

                        if let Err(e) = state.http
                            .interaction(state.application_id)
                            .create_response(interaction_id, &token, &response)
                            .await
                        {
                            eprintln!("[ERROR] Failed to send response: {}", e);
                        }
                    }
                } else {
                    send_error_response(
                        state.http.clone(),
                        state.application_id,
                        interaction_id,
                        token,
                        "This command can only be used in a server"
                    ).await?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

async fn send_error_response(
    http: Arc<HttpClient>,
    application_id: Id<twilight_model::id::marker::ApplicationMarker>,
    interaction_id: Id<twilight_model::id::marker::InteractionMarker>,
    token: String,
    message: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let response = InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(twilight_model::http::interaction::InteractionResponseData {
            content: Some(format!("❌ {}", message)),
            ..Default::default()
        }),
    };

    if let Err(e) = http
        .interaction(application_id)
        .create_response(interaction_id, &token, &response)
        .await
    {
        eprintln!("Failed to send error response: {}", e);
    }

    Ok(())
}
