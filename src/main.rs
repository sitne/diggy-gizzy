use std::{env, error::Error, num::NonZeroU64, sync::Arc, collections::HashMap};
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client as HttpClient;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    application::interaction::{Interaction, InteractionData, InteractionType},
    application::interaction::application_command::CommandOptionValue,
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
mod voice_translator;
mod transcriber;
mod summarizer;
mod translator;
mod commands;
mod user_settings;

use voice_recorder::{RecordingManager, VoiceReceiveHandler};
use voice_translator::{TranslationManager, VoiceTranslateHandler};
use transcriber::{Transcriber, transcribe_wav_file};
use summarizer::Summarizer;
use translator::Translator;
use commands::RecordingCommands;
use user_settings::UserSettingsManager;

#[derive(CommandModel, CreateCommand)]
#[command(name = "record", desc = "Join voice channel and start recording control")]
struct RecordCommand;

/// Language choices for translation
#[derive(twilight_interactions::command::CommandOption, twilight_interactions::command::CreateOption)]
enum Language {
    #[option(name = "🇯🇵 Japanese", value = "ja")]
    Japanese,
    #[option(name = "🇰🇷 Korean", value = "ko")]
    Korean,
    #[option(name = "🇺🇸 English", value = "en")]
    English,
}

/// Set language for translation command
#[derive(CommandModel, CreateCommand)]
#[command(name = "translate_set", desc = "Set your language for translation")]
struct TranslateSetCommand {
    /// Your speaking language
    source: Language,
    /// Target language for translation
    target: Language,
}

/// Start real-time voice translation
#[derive(CommandModel, CreateCommand)]
#[command(name = "translate_start", desc = "Start real-time voice translation")]
struct TranslateStartCommand;

/// Stop real-time voice translation
#[derive(CommandModel, CreateCommand)]
#[command(name = "translate_stop", desc = "Stop real-time voice translation")]
struct TranslateStopCommand;



struct BotState {
    http: Arc<HttpClient>,
    application_id: Id<twilight_model::id::marker::ApplicationMarker>,
    http_client: ReqwestClient,
    recording_commands: RecordingCommands,
    translation_manager: Arc<TranslationManager>,
    translator: Arc<Translator>,
    transcriber: Arc<Transcriber>,
    user_settings: Arc<UserSettingsManager>,
    user_voice_states: Arc<Mutex<HashMap<Id<twilight_model::id::marker::UserMarker>, Id<twilight_model::id::marker::ChannelMarker>>>>,
    songbird: Arc<Songbird>,
    voice_handlers: Arc<Mutex<HashMap<Id<twilight_model::id::marker::GuildMarker>, voice_recorder::VoiceReceiveHandler>>>,
    translate_handlers: Arc<Mutex<HashMap<Id<twilight_model::id::marker::GuildMarker>, VoiceTranslateHandler>>>,
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

    let deepl_api_key = env::var("DEEPL_API_KEY")
        .expect("DEEPL_API_KEY must be set");

    let whisper_model_path = env::var("WHISPER_MODEL_PATH")
        .unwrap_or_else(|_| "./models/ggml-base.bin".to_string());

    let whisper_model_fast_path = env::var("WHISPER_MODEL_FAST_PATH")
        .unwrap_or_else(|_| "./models/ggml-large-v3-turbo-q5_0.bin".to_string());

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
    let transcriber_fast = Arc::new(Transcriber::new(&whisper_model_fast_path)?);
    let summarizer = Arc::new(Summarizer::new(zai_api_key.clone()));
    let translation_manager = Arc::new(TranslationManager::new());
    let translator = Arc::new(Translator::new(deepl_api_key));
    let user_settings = Arc::new(UserSettingsManager::new("./user_settings.json"));

    let recording_commands = RecordingCommands::new(
        recording_manager.clone(),
        transcriber.clone(),
        summarizer,
    );

    // Register global commands using twilight-interactions
    println!("[INFO] Registering global commands...");
    let interaction_client = http.interaction(application_id);
    
    let commands = vec![
        RecordCommand::create_command().into(),
        TranslateStartCommand::create_command().into(),
        TranslateStopCommand::create_command().into(),
        TranslateSetCommand::create_command().into(),
    ];
    
    match interaction_client.set_global_commands(&commands).await {
        Ok(_) => println!("[INFO] Global commands registered successfully"),
        Err(e) => eprintln!("[ERROR] Failed to register global commands: {}", e),
    }
    
    // Note: Guild commands are automatically removed when the bot leaves a guild
    // or can be manually removed by kicking and re-inviting the bot to a guild

    let bot_state = Arc::new(BotState {
        http: http.clone(),
        application_id,
        http_client,
        recording_commands,
        translation_manager,
        translator,
        transcriber: transcriber_fast,
        user_settings,
        user_voice_states: Arc::new(Mutex::new(HashMap::new())),
        songbird: Arc::new(songbird),
        voice_handlers: Arc::new(Mutex::new(HashMap::new())),
        translate_handlers: Arc::new(Mutex::new(HashMap::new())),
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
    let interaction_id = interaction.id;
    let token = interaction.token.clone();
    let guild_id = interaction.guild_id;
    let channel_id = interaction.channel_id;
    let user_id = interaction
        .user
        .as_ref()
        .map(|u| u.id)
        .or_else(|| interaction.member.as_ref().and_then(|m| m.user.as_ref().map(|u| u.id)));
    
    if let Some(InteractionData::ApplicationCommand(ref command_data)) = interaction.data {
        match command_data.name.as_str() {
            "record" => {
                if let Some(guild_id) = guild_id {
                    if let (Some(user_id), Some(channel_id)) = (user_id, channel_id) {
                        let _user_voice_states = state.user_voice_states.lock().await;
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
            "translate_start" => {
                handle_translate_start(interaction, state).await?;
            }
            "translate_stop" => {
                handle_translate_stop(interaction, state).await?;
            }
            "translate_set" => {
                handle_translate_set(interaction, state).await?;
            }
            _ => {}
        }
    }

    Ok(())
}

async fn handle_translate_start(
    interaction: Interaction,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let interaction_id = interaction.id;
    let token = interaction.token.clone();
    let guild_id = interaction.guild_id;

    if let Some(guild_id) = guild_id {
        if state.recording_commands.recording_manager.is_recording(guild_id).await {
            send_error_response(
                state.http.clone(),
                state.application_id,
                interaction_id,
                token,
                "Cannot start translation while recording is in progress"
            ).await?;
            return Ok(());
        }

        if state.translation_manager.is_translating(guild_id).await {
            send_error_response(
                state.http.clone(),
                state.application_id,
                interaction_id,
                token,
                "Translation is already active"
            ).await?;
            return Ok(());
        }

        let user_id = interaction
            .user
            .map(|u| u.id)
            .or_else(|| interaction.member.as_ref().and_then(|m| m.user.as_ref().map(|u| u.id)));

        if let Some(user_id) = user_id {
            let voice_states = state.user_voice_states.lock().await;
            
            if let Some(voice_channel_id) = voice_states.get(&user_id).copied() {
                drop(voice_states);

                let channel_id_nz = match NonZeroU64::new(voice_channel_id.get()) {
                    Some(id) => id,
                    None => {
                        send_error_response(
                            state.http.clone(),
                            state.application_id,
                            interaction_id,
                            token,
                            "Invalid voice channel"
                        ).await?;
                        return Ok(());
                    }
                };

                let call_result = state.songbird.join(guild_id, channel_id_nz).await;

                match call_result {
                    Ok(call) => {
                        let _session = state.translation_manager
                            .start_translation(guild_id, voice_channel_id, voice_translator::TranslationPair::new("ja", "en"))
                            .await;

                        let translate_handler = VoiceTranslateHandler::new(
                            state.translation_manager.clone(),
                            guild_id,
                        );

                        let mut call_lock = call.lock().await;
                        call_lock.add_global_event(
                            SongbirdEvent::Core(CoreEvent::SpeakingStateUpdate),
                            translate_handler.clone(),
                        );
                        call_lock.add_global_event(
                            SongbirdEvent::Core(CoreEvent::VoiceTick),
                            translate_handler.clone(),
                        );
                        drop(call_lock);

                        state.translate_handlers.lock().await.insert(guild_id, translate_handler);

                        let http = state.http.clone();
                        let application_id = state.application_id;
                        let translation_manager = state.translation_manager.clone();
                        let translator = state.translator.clone();
                        let transcriber = state.transcriber.clone();
                        let user_settings = state.user_settings.clone();
                        let guild_id_for_task = guild_id;

                        tokio::spawn(async move {
                            process_translation_loop(
                                http,
                                application_id,
                                translation_manager,
                                translator,
                                transcriber,
                                user_settings,
                                guild_id_for_task,
                                voice_channel_id,
                            ).await;
                        });

                        let response = InteractionResponse {
                            kind: InteractionResponseType::ChannelMessageWithSource,
                            data: Some(twilight_model::http::interaction::InteractionResponseData {
                                content: Some("🌐 **Translation started!**\n\nUse `/translate_set <source> <target>` to configure your language pair.\n\n**Examples:**\n• `/translate_set ja ko` - Japanese to Korean\n• `/translate_set ko ja` - Korean to Japanese\n• `/translate_set en ja` - English to Japanese".to_string()),
                                ..Default::default()
                            }),
                        };

                        state.http
                            .interaction(state.application_id)
                            .create_response(interaction_id, &token, &response)
                            .await?;
                    }
                    Err(e) => {
                        eprintln!("[ERROR] Failed to join voice channel: {:?}", e);
                        send_error_response(
                            state.http.clone(),
                            state.application_id,
                            interaction_id,
                            token,
                            &format!("Failed to join voice channel: {}", e)
                        ).await?;
                    }
                }
            } else {
                send_error_response(
                    state.http.clone(),
                    state.application_id,
                    interaction_id,
                    token,
                    "You must be in a voice channel"
                ).await?;
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

    Ok(())
}

async fn handle_translate_stop(
    interaction: Interaction,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let interaction_id = interaction.id;
    let token = interaction.token.clone();
    let guild_id = interaction.guild_id;

    if let Some(guild_id) = guild_id {
        if !state.translation_manager.is_translating(guild_id).await {
            send_error_response(
                state.http.clone(),
                state.application_id,
                interaction_id,
                token,
                "No active translation session"
            ).await?;
            return Ok(());
        }

        state.translation_manager.stop_translation(guild_id).await;
        state.translate_handlers.lock().await.remove(&guild_id);

        if let Err(e) = state.songbird.leave(guild_id).await {
            eprintln!("[ERROR] Failed to leave voice channel: {}", e);
        }

        let response = InteractionResponse {
            kind: InteractionResponseType::ChannelMessageWithSource,
            data: Some(twilight_model::http::interaction::InteractionResponseData {
                content: Some("✅ **Translation stopped!**".to_string()),
                ..Default::default()
            }),
        };

        state.http
            .interaction(state.application_id)
            .create_response(interaction_id, &token, &response)
            .await?;
    } else {
        send_error_response(
            state.http.clone(),
            state.application_id,
            interaction_id,
            token,
            "This command can only be used in a server"
        ).await?;
    }

    Ok(())
}

async fn handle_translate_set(
    interaction: Interaction,
    state: Arc<BotState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let interaction_id = interaction.id;
    let token = interaction.token.clone();
    
    let user_id = interaction
        .user
        .map(|u| u.id)
        .or_else(|| interaction.member.as_ref().and_then(|m| m.user.as_ref().map(|u| u.id)));

    if let Some(user_id) = user_id {
        if let Some(InteractionData::ApplicationCommand(command_data)) = interaction.data {
            let mut source_lang = None;
            let mut target_lang = None;
            
            for option in &command_data.options {
                match option.name.as_str() {
                    "source" => {
                        if let CommandOptionValue::String(val) = &option.value {
                            source_lang = Some(val.as_str());
                        }
                    }
                    "target" => {
                        if let CommandOptionValue::String(val) = &option.value {
                            target_lang = Some(val.as_str());
                        }
                    }
                    _ => {}
                }
            }
            
            let (source, target) = match (source_lang, target_lang) {
                (Some(s), Some(t)) => (s, t),
                _ => {
                    send_error_response(
                        state.http.clone(),
                        state.application_id,
                        interaction_id,
                        token,
                        "Please select both source and target languages"
                    ).await?;
                    return Ok(());
                }
            };
            
            let valid_langs = ["ja", "ko", "en"];
            if !valid_langs.contains(&source) || !valid_langs.contains(&target) {
                send_error_response(
                    state.http.clone(),
                    state.application_id,
                    interaction_id,
                    token,
                    "Invalid language codes. Use: ja, ko, or en"
                ).await?;
                return Ok(());
            }

            state.user_settings.set_user_language(user_id, source, target).await;

            let flag = |lang: &str| match lang {
                "ja" => "🇯🇵",
                "ko" => "🇰🇷",
                "en" => "🇺🇸",
                _ => "🌐",
            };

            let lang_name = |lang: &str| -> String {
                match lang {
                    "ja" => "Japanese".to_string(),
                    "ko" => "Korean".to_string(),
                    "en" => "English".to_string(),
                    _ => lang.to_string(),
                }
            };

            let response = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(twilight_model::http::interaction::InteractionResponseData {
                    content: Some(format!(
                        "✅ **Language setting saved!**\n\n{} **Speaking**: {}\n{} **Translation target**: {}",
                        flag(source),
                        lang_name(source),
                        flag(target),
                        lang_name(target)
                    )),
                    ..Default::default()
                }),
            };

            state.http
                .interaction(state.application_id)
                .create_response(interaction_id, &token, &response)
                .await?;
        }
    } else {
        send_error_response(
            state.http.clone(),
            state.application_id,
            interaction_id,
            token,
            "Could not identify user"
        ).await?;
    }

    Ok(())
}

async fn process_translation_loop(
    http: Arc<HttpClient>,
    _application_id: Id<twilight_model::id::marker::ApplicationMarker>,
    translation_manager: Arc<TranslationManager>,
    translator: Arc<Translator>,
    transcriber: Arc<Transcriber>,
    user_settings: Arc<UserSettingsManager>,
    guild_id: Id<twilight_model::id::marker::GuildMarker>,
    voice_channel_id: Id<twilight_model::id::marker::ChannelMarker>,
) {
    use twilight_model::channel::message::embed::Embed;
    use twilight_model::channel::message::embed::EmbedField;
    use transcriber::convert_i16_to_f32;
    use transcriber::downsample_48k_to_16k;
    use std::time::Instant;

    loop {
        if !translation_manager.is_translating(guild_id).await {
            break;
        }

        let ready_buffers = translation_manager.get_ready_translations(guild_id).await;

        for (user_id, samples) in ready_buffers {
            let http = http.clone();
            let translator = translator.clone();
            let transcriber = transcriber.clone();
            let user_settings = user_settings.clone();
            let voice_channel_id = voice_channel_id;

            tokio::spawn(async move {
                let user_setting = match user_settings.get_user_setting(user_id).await {
                    Some(setting) => setting,
                    None => {
                        println!("[INFO] Skipping user {} - no language settings", user_id);
                        return;
                    }
                };

                if samples.len() < 24000 {
                    return;
                }

                let total_start = Instant::now();
                let convert_start = Instant::now();
                let samples_f32 = convert_i16_to_f32(&samples);
                let final_samples = downsample_48k_to_16k(&samples_f32);
                let convert_time = convert_start.elapsed();
                
                let transcribe_start = Instant::now();
                match transcriber.transcribe_with_language(&final_samples, Some(&user_setting.source_lang)) {
                    Ok((transcription, _)) => {
                        let transcribe_time = transcribe_start.elapsed();
                        if !transcription.trim().is_empty() {
                            let source_full = user_setting.get_source_full();
                            let target_full = user_setting.get_target_full();
                            
                            let translate_start = Instant::now();
                            match translator.translate(&transcription, &source_full, &target_full).await {
                                Ok(translated) => {
                                    let translate_time = translate_start.elapsed();
                                    let total_time = total_start.elapsed();
                                    println!("[PERF] Convert: {:?}, Transcribe: {:?}, Translate: {:?}, Total: {:?}", convert_time, transcribe_time, translate_time, total_time);
                                    
                                    let embed = Embed {
                                        author: None,
                                        color: Some(0x3498db),
                                        description: None,
                                        fields: vec![
                                            EmbedField {
                                                inline: false,
                                                name: format!("🗣️ Original ({})", user_setting.source_lang.to_uppercase()),
                                                value: transcription,
                                            },
                                            EmbedField {
                                                inline: false,
                                                name: format!("🌐 Translation ({})", user_setting.target_lang.to_uppercase()),
                                                value: translated,
                                            },
                                        ],
                                        footer: None,
                                        image: None,
                                        kind: "rich".to_string(),
                                        provider: None,
                                        thumbnail: None,
                                        timestamp: None,
                                        title: Some("Real-time Translation".to_string()),
                                        url: None,
                                        video: None,
                                    };

                                    let _ = http.create_message(voice_channel_id)
                                        .embeds(&[embed])
                                        .await;
                                }
                                Err(e) => {
                                    eprintln!("[ERROR] Translation failed: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[ERROR] Transcription failed: {}", e);
                    }
                }
            });
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
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
