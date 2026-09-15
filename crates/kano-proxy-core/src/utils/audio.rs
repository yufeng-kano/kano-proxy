//! OpenAI / OpenRouter `input_audio` content parts (apps/api/src/utils/audio.ts,
//! docs/api.md § Audio input).
//!
//! Only conversion targets need a mime: a passthrough provider forwards the client's part
//! untouched and its upstream reads `format` itself.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// `format` token → mime, limited to what the Gemini wire accepts.
const MIME_BY_FORMAT: &[(&str, &str)] = &[
    ("wav", "audio/wav"),
    ("mp3", "audio/mp3"),
    ("mpeg", "audio/mp3"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("aiff", "audio/aiff"),
    // Apple's recorders (Voice Memos, `say`, iPhone) hand out `.m4a`: AAC in an MP4
    // container, which the backend reads as `audio/mp4`, not `audio/aac` (verified against
    // the live Gemini backend 2026-08-24).
    ("m4a", "audio/mp4"),
    ("mp4", "audio/mp4"),
];

/// Human list for the `unsupported_audio_format` error — kept next to the map it describes.
pub const SUPPORTED_AUDIO_FORMATS: &str = "wav, mp3, m4a, aac, flac, ogg, opus or aiff";

static DATA_URL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)^data:([^;,]+);base64,(.+)$").expect("data url regex"));

/// Inline audio bytes plus the mime the conversion targets name them with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInline {
    pub mime_type: String,
    pub data: String,
}

/// Result of [`scan_audio_parts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioScan {
    pub present: bool,
    pub convertible: bool,
}

fn mime_for_format(format: &str) -> Option<&'static str> {
    MIME_BY_FORMAT.iter().find(|(k, _)| *k == format).map(|(_, v)| *v)
}

/// An `input_audio` part as inline data, or `None` when the part is not audio or carries
/// nothing this proxy can name a mime for. A `data:` URL's own mime wins over `format`: the
/// client stated it explicitly.
pub fn audio_inline(part: &Value) -> Option<AudioInline> {
    if !part.is_object() {
        return None;
    }
    if part.get("type").and_then(Value::as_str) != Some("input_audio") {
        return None;
    }
    let input_audio = part.get("input_audio");
    let raw = input_audio
        .and_then(|v| v.get("data"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return None;
    }
    if let Some(caps) = DATA_URL_RE.captures(raw) {
        return Some(AudioInline {
            mime_type: caps[1].to_string(),
            data: caps[2].to_string(),
        });
    }
    let format = input_audio
        .and_then(|v| v.get("format"))
        .and_then(Value::as_str)
        .map(|f| f.to_lowercase())
        .unwrap_or_default();
    mime_for_format(&format).map(|mime| AudioInline { mime_type: mime.to_string(), data: raw.to_string() })
}

/// Walk an OpenAI-shaped message list once: does it carry audio at all, and is every audio
/// part in it convertible? The route needs both before it can decide between dispatching,
/// `unsupported_modality` and `unsupported_audio_format` (docs/api.md § Audio input).
pub fn scan_audio_parts(messages: &[Value]) -> AudioScan {
    let mut present = false;
    let mut convertible = true;
    for message in messages {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if !part.is_object() {
                continue;
            }
            if part.get("type").and_then(Value::as_str) != Some("input_audio") {
                continue;
            }
            present = true;
            if audio_inline(part).is_none() {
                convertible = false;
            }
        }
    }
    AudioScan { present, convertible }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_a_format_token_to_its_mime() {
        let part = json!({ "type": "input_audio", "input_audio": { "data": "UklGRg==", "format": "wav" } });
        assert_eq!(
            audio_inline(&part),
            Some(AudioInline { mime_type: "audio/wav".into(), data: "UklGRg==".into() })
        );
    }

    #[test]
    fn folds_the_mp3_and_mpeg_spellings_onto_one_mime() {
        for format in ["mp3", "mpeg", "MP3"] {
            let part = json!({ "type": "input_audio", "input_audio": { "data": "x", "format": format } });
            assert_eq!(audio_inline(&part).unwrap().mime_type, "audio/mp3");
        }
    }

    #[test]
    fn reads_m4a_as_the_mp4_container_it_is() {
        let part = json!({ "type": "input_audio", "input_audio": { "data": "x", "format": "m4a" } });
        assert_eq!(audio_inline(&part).unwrap().mime_type, "audio/mp4");
    }

    #[test]
    fn prefers_a_data_url_mime_over_the_format_field() {
        let part = json!({
            "type": "input_audio",
            "input_audio": { "data": "data:audio/ogg;base64,AAAA", "format": "wav" },
        });
        assert_eq!(
            audio_inline(&part),
            Some(AudioInline { mime_type: "audio/ogg".into(), data: "AAAA".into() })
        );
    }

    #[test]
    fn rejects_a_non_audio_part_empty_data_and_an_unknown_format() {
        assert_eq!(audio_inline(&json!({ "type": "text", "text": "hi" })), None);
        assert_eq!(audio_inline(&json!("not an object")), None);
        assert_eq!(
            audio_inline(&json!({ "type": "input_audio", "input_audio": { "data": "   ", "format": "wav" } })),
            None
        );
        assert_eq!(
            audio_inline(&json!({ "type": "input_audio", "input_audio": { "data": "x", "format": "amr" } })),
            None
        );
        assert_eq!(audio_inline(&json!({ "type": "input_audio", "input_audio": { "data": "x" } })), None);
    }

    #[test]
    fn scan_reports_absence_for_text_only_histories() {
        let messages = vec![
            json!({ "role": "user", "content": "plain text" }),
            json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] }),
        ];
        assert_eq!(scan_audio_parts(&messages), AudioScan { present: false, convertible: true });
    }

    #[test]
    fn scan_reports_present_and_convertible() {
        let messages = vec![json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "what is this" },
                { "type": "input_audio", "input_audio": { "data": "x", "format": "wav" } },
            ],
        })];
        assert_eq!(scan_audio_parts(&messages), AudioScan { present: true, convertible: true });
    }

    #[test]
    fn scan_reports_an_unconvertible_part() {
        let messages = vec![json!({
            "role": "user",
            "content": [
                { "type": "input_audio", "input_audio": { "data": "x", "format": "wav" } },
                { "type": "input_audio", "input_audio": { "data": "x", "format": "amr" } },
            ],
        })];
        assert_eq!(scan_audio_parts(&messages), AudioScan { present: true, convertible: false });
    }
}
