#[cfg(feature = "ml")]

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "ml")]
    {
        println!("Downloading and caching HuggingFace NER model weights...");
        let _engine = matchmaker_orchestrator::pii_scrubber::NerEngine::new()?;
        println!("Successfully cached model weights.");
    }
    Ok(())
}
