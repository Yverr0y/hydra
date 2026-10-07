//! Shared audio conversion for accepted plugin media plans.

use std::path::Path;
use std::process::Stdio;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};

/// Converts an audio track without a shell; cancellation terminates ffmpeg.
///
/// # Errors
/// Returns an error for unsupported formats, missing ffmpeg, failed conversion or cancellation.
pub async fn extract_audio(
    input: &Path,
    output: &Path,
    format: &str,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<(), String> {
    let codec = match format {
        "mp3" => "libmp3lame",
        "m4a" => "aac",
        "opus" => "libopus",
        "flac" => "flac",
        "wav" => "pcm_s16le",
        _ => return Err(format!("unsupported audio format: {format}")),
    };
    if cancel
        .as_ref()
        .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    {
        return Err("audio extraction cancelled".into());
    }
    let mut command = tokio::process::Command::new("ffmpeg");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let child = command
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-map", "0:a:0", "-c:a", codec])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("audio extraction requires ffmpeg: {e}"))?;
    let result = child.wait_with_output();
    tokio::pin!(result);
    loop {
        tokio::select! {
            output = &mut result => {
                let output = output.map_err(|e| e.to_string())?;
                if output.status.success() { return Ok(()); }
                return Err(format!("audio extraction failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return Err("audio extraction cancelled".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_before_conversion_starts_no_child() {
        let cancel = Arc::new(AtomicBool::new(true));
        assert_eq!(
            extract_audio(
                Path::new("missing"),
                Path::new("out.mp3"),
                "mp3",
                Some(cancel)
            )
            .await
            .unwrap_err(),
            "audio extraction cancelled"
        );
    }

    #[tokio::test]
    async fn converts_audio_formats_and_reports_invalid_input() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.wav");
        let samples = 2000u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.resize(wav.len() + samples as usize * 2, 0);
        std::fs::write(&input, wav).unwrap();
        for format in ["mp3", "m4a", "opus", "flac", "wav"] {
            let output = root.path().join(format!("converted.{format}"));
            extract_audio(&input, &output, format, None).await.unwrap();
            assert!(std::fs::metadata(output).unwrap().len() > 0);
        }
        assert!(extract_audio(
            &root.path().join("missing"),
            &root.path().join("out.mp3"),
            "mp3",
            None
        )
        .await
        .unwrap_err()
        .contains("audio extraction failed"));
    }

    #[tokio::test]
    async fn unsupported_formats_are_refused_before_starting_a_tool() {
        assert!(
            extract_audio(Path::new("missing"), Path::new("out"), "invalid", None)
                .await
                .unwrap_err()
                .contains("unsupported audio format")
        );
    }
}
