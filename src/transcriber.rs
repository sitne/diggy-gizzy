use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};
use std::path::Path;

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
        if audio_data.is_empty() {
            return Ok(String::new());
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
        params.set_print_timestamps(false);

        state.full(params, audio_data)?;

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
