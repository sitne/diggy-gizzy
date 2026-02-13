use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};
use std::path::Path;

const LANGUAGE_CODES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv",
    "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no",
    "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr",
    "az", "sl", "kn", "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw",
    "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu",
    "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl",
    "mg", "as", "tt", "haw", "ln", "ha", "ba", "jw", "su",
];

fn get_lang_str_from_id(lang_id: i32) -> &'static str {
    LANGUAGE_CODES.get(lang_id as usize).copied().unwrap_or("en")
}

pub struct Transcriber {
    ctx: WhisperContext,
}

impl Transcriber {
    pub fn new(model_path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if !Path::new(model_path).exists() {
            return Err(format!("Whisper model not found at: {}", model_path).into());
        }

        let ctx = WhisperContext::new_with_params(
            model_path,
            WhisperContextParameters::default(),
        )?;

        Ok(Self { ctx })
    }

    pub fn transcribe(&self, audio_data: &[f32], language: Option<&str>) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let (text, _) = self.transcribe_with_language(audio_data, language)?;
        Ok(text)
    }

    /// Transcribe audio and return (text, detected_language_code)
    /// If language is None, auto-detects the language
    pub fn transcribe_with_language(&self, audio_data: &[f32], language: Option<&str>) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        if audio_data.is_empty() {
            return Ok((String::new(), "en".to_string()));
        }

        // First pass: auto-detect language
        let detected_lang = if let Some(lang) = language {
            lang.to_string()
        } else {
            let mut state = self.ctx.create_state()?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            
            // First pass without language hint to detect language
            params.set_translate(false);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_no_context(true);
            params.set_suppress_blank(true);
            params.set_suppress_nst(true);
            params.set_temperature(0.0);
            params.set_no_speech_thold(0.6);
            
            state.full(params, audio_data)?;
            
            match state.lang_detect(0, 4) {
                Ok((lang_id, _probs)) => {
                    get_lang_str_from_id(lang_id).to_string()
                }
                Err(_) => {
                    // Fallback to local detection based on text content
                    let text = self.extract_text(&state)?;
                    Self::detect_language_local(&text)
                }
            }
        };

        // Second pass: transcribe with detected language
        let mut state = self.ctx.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        
        // Set the detected language for transcription
        params.set_language(Some(&detected_lang));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_temperature(0.0);
        params.set_no_speech_thold(0.6);

        state.full(params, audio_data)?;
        let transcription = self.extract_text(&state)?;
        
        Ok((transcription, detected_lang))
    }

    fn extract_text(&self, state: &whisper_rs::WhisperState) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let num_segments = state.full_n_segments()?;
        let mut transcription = String::new();

        for i in 0..num_segments {
            let text = state.full_get_segment_text(i)?;
            if !text.trim().is_empty() {
                transcription.push_str(&text);
                transcription.push(' ');
            }
        }

        Ok(transcription.trim().to_string())
    }

    /// Fallback local language detection based on character types
    fn detect_language_local(text: &str) -> String {
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
            "ja".to_string()
        } else {
            "en".to_string()
        }
    }

    pub fn transcribe_with_timestamps(&self, audio_data: &[f32], language: Option<&str>) -> Result<Vec<(i64, i64, String)>, Box<dyn std::error::Error + Send + Sync>> {
        if audio_data.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = self.ctx.create_state()?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        
        if let Some(lang) = language {
            params.set_language(Some(lang));
        }
        
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(true);

        state.full(params, audio_data)?;

        let num_segments = state.full_n_segments()?;
        let mut segments = Vec::new();

        for i in 0..num_segments {
            let text = state.full_get_segment_text(i)?;
            let start = state.full_get_segment_t0(i)?;
            let end = state.full_get_segment_t1(i)?;
            
            if !text.trim().is_empty() {
                segments.push((start, end, text));
            }
        }

        Ok(segments)
    }
}

pub fn convert_i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter()
        .map(|&s| s as f32 / 32768.0)
        .collect()
}

pub fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f32 = samples.iter().map(|s| s * s).sum();
    let mean = sum_squares / samples.len() as f32;
    mean.sqrt()
}

pub fn is_likely_hallucination(text: &str, duration_ms: u64, rms: f32) -> bool {
    let normalized: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !"。、！!？?".contains(*c))
        .collect();

    let short_audio = duration_ms < 1200;
    let low_energy = rms < 0.01;

    if !(short_audio || low_energy) {
        return false;
    }

    let known_phrases = [
        "お疲れ様でした",
        "おつかれさまでした",
        "ご視聴ありがとうございました",
        "ごしちょうありがとうございました",
    ];

    known_phrases.iter().any(|phrase| normalized.contains(phrase))
}

pub fn downsample_48k_to_16k(samples: &[f32]) -> Vec<f32> {
    samples.iter()
        .step_by(3)
        .copied()
        .collect()
}

pub async fn transcribe_wav_file(
    transcriber: &Transcriber,
    wav_path: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use hound::WavReader;
    
    let mut reader = WavReader::open(wav_path)?;
    let spec = reader.spec();
    
    let samples: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
    let samples_f32 = convert_i16_to_f32(&samples);
    
    let final_samples = if spec.sample_rate == 48000 {
        downsample_48k_to_16k(&samples_f32)
    } else if spec.sample_rate == 16000 {
        samples_f32
    } else {
        return Err(format!("Unsupported sample rate: {}", spec.sample_rate).into());
    };

    transcriber.transcribe(&final_samples, Some("ja"))
}
