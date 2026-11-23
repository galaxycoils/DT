use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_DURATION: Duration = Duration::from_secs(10);

pub struct Recording {
    stream: cpal::Stream,
    pcm: Arc<Mutex<Vec<i16>>>,
    error: Arc<Mutex<Option<String>>>,
    sample_rate: u32,
    started: Instant,
}

impl Recording {
    pub fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or("No microphone input device found")?;
        let supported = device
            .default_input_config()
            .map_err(|error| error.to_string())?;
        let format = supported.sample_format();
        let config = supported.config();
        let sample_rate = config.sample_rate.0;
        let pcm = Arc::new(Mutex::new(Vec::with_capacity(
            sample_rate as usize * MAX_DURATION.as_secs() as usize,
        )));
        let error = Arc::new(Mutex::new(None));
        let stream = match format {
            SampleFormat::F32 => input::<f32>(&device, &config, &pcm, &error),
            SampleFormat::I16 => input::<i16>(&device, &config, &pcm, &error),
            SampleFormat::U16 => input::<u16>(&device, &config, &pcm, &error),
            SampleFormat::I32 => input::<i32>(&device, &config, &pcm, &error),
            _ => return Err(format!("Unsupported microphone sample format {format}")),
        }
        .map_err(|problem| problem.to_string())?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Self {
            stream,
            pcm,
            error,
            sample_rate,
            started: Instant::now(),
        })
    }

    pub fn expired(&self) -> bool {
        self.started.elapsed() >= MAX_DURATION
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        drop(self.stream);
        let error = self.error.lock().map_err(|e| e.to_string())?;
        if let Some(error) = error.as_ref() {
            return Err(error.clone());
        }
        let pcm = self.pcm.lock().map_err(|e| e.to_string())?;
        encode_wave(&pcm, self.sample_rate)
    }
}

fn input<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    pcm: &Arc<Mutex<Vec<i16>>>,
    error: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    f32: FromSample<T>,
{
    let pcm = pcm.clone();
    let error = error.clone();
    let channels = usize::from(config.channels);
    let max_samples = config.sample_rate.0 as usize * MAX_DURATION.as_secs() as usize;
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            if let Ok(mut pcm) = pcm.try_lock() {
                let remaining = max_samples.saturating_sub(pcm.len());
                pcm.extend(data.chunks_exact(channels).take(remaining).map(|frame| {
                    let mono = frame
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    i16::from_sample(mono.clamp(-1.0, 1.0))
                }));
            }
        },
        move |problem| {
            if let Ok(mut error) = error.lock() {
                *error = Some(problem.to_string());
            }
        },
        None,
    )
}

fn encode_wave(pcm: &[i16], sample_rate: u32) -> Result<Vec<u8>, String> {
    if pcm.is_empty() {
        return Err("No microphone audio was captured".into());
    }
    let mut output = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(&mut output, spec).map_err(|e| e.to_string())?;
    for &sample in pcm {
        writer.write_sample(sample).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    #[test]
    fn encode_recording_for_transcription() {
        let samples = [-32768, -14, 0, 14, 32767];
        let wav = super::encode_wave(&samples, 48000).unwrap();
        let mut reader = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap();
        assert_eq!(reader.spec().sample_rate, 48000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            samples
        );
        assert!(super::encode_wave(&[], 48000).is_err());
    }
}
