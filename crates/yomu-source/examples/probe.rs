//! Read-only source diagnostic, including the first image of the oldest unit.
//! Usage: probe SOURCE.toml TITLE_URL [SOLVER_ORIGIN]
//! Source opt-in/allowed_hosts are read from SOURCE.toml, never CLI globals.
use std::sync::Arc;
use yomu_source::{
    Source,
    flaresolverr::{FlareSolverr, FlareSolverrConfig},
    selector::{SelectorSource, SelectorSpec},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 && args.len() != 3 {
        return Err("usage: probe SOURCE.toml TITLE_URL [SOLVER_ORIGIN]".into());
    }
    let spec: SelectorSpec = toml::from_str(&std::fs::read_to_string(&args[0])?)?;
    let mut source = SelectorSource::new(spec)?;
    if args.len() == 3 {
        source = source.with_flaresolverr(Arc::new(FlareSolverr::new(FlareSolverrConfig {
            url: args[2].parse()?,
            timeout_secs: 60,
        })?));
    }
    let details = source.manga(&args[1]).await?;
    println!(
        "Title: {}\nChapters: {}",
        details.summary.title,
        details.chapters.len()
    );
    let oldest = details
        .chapters
        .iter()
        .max_by_key(|unit| unit.source_order)
        .ok_or("no chapters")?;
    println!("Probing oldest chapter: {}", oldest.title);
    let pages = source.pages(&oldest.key).await?;
    println!("Page images: {}", pages.len());
    let image = source.image(pages.first().ok_or("no pages")?).await?;
    if !image.content_type.starts_with("image/") {
        return Err("upstream did not return an image".into());
    }
    println!(
        "First image: {} ({} bytes)",
        image.content_type,
        image.bytes.len()
    );
    Ok(())
}
