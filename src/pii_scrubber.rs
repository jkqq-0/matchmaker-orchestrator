use regex::Regex;
#[cfg(feature = "ml")]
use rust_bert::pipelines::token_classification::{TokenClassificationModel, TokenClassificationConfig};
use lazy_static::lazy_static;

lazy_static! {
    static ref EMAIL_REGEX: Regex = Regex::new(r"(?i)[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]*[a-z0-9])?").unwrap();
    static ref PHONE_REGEX: Regex = Regex::new(r"(?i)(?:(?:\+?1\s*(?:[.-]\s*)?)?(?:\(\s*([2-9]1[02-9]|[2-9][02-8]1|[2-9][02-8][02-9])\s*\)|([2-9]1[02-9]|[2-9][02-8]1|[2-9][02-8][02-9]))\s*(?:[.-]\s*)?)?([2-9]1[02-9]|[2-9][02-9]1|[2-9][02-9]{2})\s*(?:[.-]\s*)?([0-9]{4})(?:\s*(?:#|x\.?|ext\.?|extension)\s*(\d+))?").unwrap();
    static ref URL_REGEX: Regex = Regex::new(r"(?i)\b((?:https?://|www\d{0,3}[.]|[a-z0-9.\-]+[.][a-z]{2,4}/)(?:[^\s()<>]+|\(([^\s()<>]+|(\([^\s()<>]+\)))*\))+(?:\(([^\s()<>]+|(\([^\s()<>]+\)))*\)|[^\s`!()\[\]{};:'.,<>?«»“”‘’]))").unwrap();
}

pub struct NerEngine {
    #[cfg(feature = "ml")]
    model: std::sync::Mutex<TokenClassificationModel>,
}

impl NerEngine {
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(feature = "ml")]
        {
            use rust_bert::pipelines::common::{ModelType, ModelResource};
            use rust_bert::resources::RemoteResource;

            let mut config = TokenClassificationConfig::default();
            config.model_type = ModelType::Bert;
            config.model_resource = ModelResource::Torch(Box::new(RemoteResource::from_pretrained((
                "dslim/bert-base", 
                "https://huggingface.co/dslim/bert-base-NER/resolve/main/pytorch_model.bin"
            ))));
            config.config_resource = Box::new(RemoteResource::from_pretrained((
                "dslim/bert-base", 
                "https://huggingface.co/dslim/bert-base-NER/resolve/main/config.json"
            )));
            config.vocab_resource = Box::new(RemoteResource::from_pretrained((
                "dslim/bert-base", 
                "https://huggingface.co/dslim/bert-base-NER/resolve/main/vocab.txt"
            )));
            config.lower_case = false;

            let model = TokenClassificationModel::new(config)?;
            Ok(Self { model: std::sync::Mutex::new(model) })
        }
        #[cfg(not(feature = "ml"))]
        {
            Ok(Self {})
        }
    }

    pub fn scrub_entities(&self, text: &str) -> String {
        #[cfg(feature = "ml")]
        {
            // Lock the mutex to strictly serialize ML operations.
            // This prevents concurrent memory allocations inside LibTorch!
            let predictions_list = self.model.lock().unwrap().predict(&[text], true, true);
            
            let mut scrubbed_text = text.to_string();
            
            if let Some(predictions) = predictions_list.into_iter().next() {
                // We iterate in reverse so that index shifts don't break subsequent replacements
                let mut entities: Vec<_> = predictions.into_iter().collect();
                entities.sort_by(|a, b| {
                    let a_start = a.offset.as_ref().map(|o| o.begin).unwrap_or(0);
                    let b_start = b.offset.as_ref().map(|o| o.begin).unwrap_or(0);
                    b_start.cmp(&a_start)
                });

                for entity in entities {
                    // Focus on PERSON, LOC, and GPE tags
                    let label = if entity.score > 0.6 {
                        if entity.label.ends_with("PER") || entity.label.ends_with("PERSON") {
                            Some("[PERSON]")
                        } else if entity.label.ends_with("LOC") || entity.label.ends_with("GPE") {
                            Some("[LOCATION]")
                        } else if entity.label.ends_with("ORG") {
                            Some("[ORGANIZATION]")
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(mask) = label {
                        let start = entity.offset.as_ref().map(|o| o.begin as usize).unwrap_or(0);
                        let end = entity.offset.as_ref().map(|o| o.end as usize).unwrap_or(0);
                        
                        // Ensure valid range before replacing to prevent panics
                        if start < end && end <= scrubbed_text.len() {
                            scrubbed_text.replace_range(start..end, mask);
                        }
                    }
                }
            }
            
            scrubbed_text
        }
        #[cfg(not(feature = "ml"))]
        {
            text.to_string()
        }
    }
}

pub fn scrub_text_sync(mut text: String, ner_engine: &NerEngine) -> String {
    // 1. Regex Replacements
    text = EMAIL_REGEX.replace_all(&text, "[EMAIL]").to_string();
    text = PHONE_REGEX.replace_all(&text, "[PHONE]").to_string();
    text = URL_REGEX.replace_all(&text, "[URL]").to_string();

    // 2. NER Replacements
    ner_engine.scrub_entities(&text)
}
