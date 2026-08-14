mod native;
mod streaming;

use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::{
    backend::{AsrBackend, AsrSessionHandle, AsrSessionOptions},
    config::{Audio3VocabularyTerm, Config, Language},
};

pub struct QwenAudio3Backend;

impl QwenAudio3Backend {
    pub fn new() -> Self {
        Self
    }
}

impl AsrBackend for QwenAudio3Backend {
    fn spawn_session(
        &self,
        config: &Config,
        options: AsrSessionOptions,
    ) -> Result<AsrSessionHandle> {
        streaming::spawn_session(config, options)
    }

    fn transcribe_file(&self, config: &Config, wav_path: &Path) -> Result<String> {
        Ok(native::transcribe_full_audio(config, wav_path)?.unwrap_or_default())
    }
}

pub(crate) use native::transcribe_full_audio;

fn language_hints(language: Language) -> Value {
    json!(language.audio3_language_hints())
}

fn vocabulary_value(vocabulary: &[Audio3VocabularyTerm]) -> Option<Value> {
    if vocabulary.is_empty() {
        return None;
    }
    let mut values = Map::new();
    for entry in vocabulary {
        values.insert(entry.term.trim().to_owned(), json!(entry.weight));
    }
    Some(Value::Object(values))
}
