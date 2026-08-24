use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
};
use std::fs;

use clap::{Parser, Subcommand};
use rayon::prelude::*;
use serde::Serialize;
use tera::Tera;
use walkdir::{DirEntry, WalkDir};

use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, trace};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

const IMAGE_DIRECTORY: &str = "originals";
const PUBLIC_DIRECTORY: &str = "public";

#[derive(Debug)]
struct SourcePhoto {
    path: PathBuf,
    stem: String,
    hash: String,
}

impl SourcePhoto {
    fn short_hash(&self) -> &str {
        &self.hash[..12]
    }
}

#[derive(Debug)]
struct RenderedPhoto {
    _path: PathBuf,
    url: String,
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct ProcessedPhoto {
    source: SourcePhoto,
    thumb: RenderedPhoto,
    full: RenderedPhoto,
}

#[derive(Debug, Serialize)]
struct TemplatePhoto {
    href: String,
    width: u32,
    height: u32,
    src: String,
    alt: String,
}

impl TemplatePhoto {
    const PHOTO_URL_ORIGIN: &str = "https://assets.domino.garden/file/domino-garden-public";

    fn asset_url(url: &str) -> String {
        Self::PHOTO_URL_ORIGIN.to_string() + url
    }
}

impl From<ProcessedPhoto> for TemplatePhoto {
    fn from(value: ProcessedPhoto) -> Self {
        TemplatePhoto {
            href: TemplatePhoto::asset_url(&value.full.url),
            width: value.full.width,
            height: value.full.height,
            src: TemplatePhoto::asset_url(&value.thumb.url),
            alt: format!("domino photo {}", value.source.short_hash()),
        }
    }
}

#[derive(Debug)]
enum ImageType {
    Thumb,
    Full,
}

impl ImageType {
    fn public_subdirectory(&self) -> &str {
        match self {
            ImageType::Thumb => "thumb",
            ImageType::Full => "full",
        }
    }
}

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    Build {},
    Deploy {},
}

impl CliCommand {
    #[tracing::instrument(err)]
    fn run(&self) -> anyhow::Result<()> {
        match self {
            CliCommand::Build {} => build(),
            CliCommand::Deploy {} => deploy_photos(),
        }
    }
}

fn image_file(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.to_ascii_lowercase().ends_with(".jpg"))
        .unwrap_or(false)
}

#[tracing::instrument(skip_all, fields(path = %path.display()))]
fn hash_photo(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    let hash = blake3::hash(&bytes);
    Ok(hash.to_string())
}

fn add_imagemagick_args(
    command: &mut Command,
    image_type: &ImageType,
    input: &Path,
    output: &Path,
) {
    command.arg(input).arg("-auto-orient").arg("-strip");

    match image_type {
        ImageType::Thumb => {
            command
                .arg("-resize")
                .arg("500x500^")
                .arg("-gravity")
                .arg("center")
                .arg("-extent")
                .arg("500x500")
                .arg("-quality")
                .arg("80");
        }
        ImageType::Full => {
            command
                .arg("-resize")
                .arg("1600x1600>")
                .arg("-quality")
                .arg("82");
        }
    }

    command.arg(output);
}

#[tracing::instrument(err)]
fn image_dimensions(path: &Path) -> anyhow::Result<(u32, u32)> {
    let output = Command::new("magick")
        .arg("identify")
        .arg("-format")
        .arg("%w %h")
        .arg(path)
        .env("MAGICK_THREAD_LIMIT", "1")
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "magick identify failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let (width, height) = stdout
        .trim()
        .split_once(' ')
        .ok_or(anyhow::anyhow!("unexpected identify output: {stdout:?}"))?;

    Ok((width.parse()?, height.parse()?))
}

#[tracing::instrument(err)]
fn deploy_photos() -> anyhow::Result<()> {
    tracing::info!("deploying photos");
    let output = std::process::Command::new("rclone")
        .arg("sync")
        .arg("public/photos/")
        .arg("b2:domino-garden-public/photos")
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "rclone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    tracing::info!("deploying successful");
    Ok(())
}

#[tracing::instrument(skip(photo), fields(hash = photo.short_hash(), ?image_type))]
fn process_photo(photo: &SourcePhoto, image_type: ImageType) -> anyhow::Result<RenderedPhoto> {
    let hash = &photo.short_hash();
    let public_path = PathBuf::from(PUBLIC_DIRECTORY)
        .join("photos")
        .join(image_type.public_subdirectory())
        .join(format!("{hash}.webp"));

    let url_path = public_path.strip_prefix("public")?;
    let url = format!(
        "/{}",
        url_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("public URL path is not valid UTF-8"))?
    );

    if !public_path.exists() {
        let input = &photo.path;
        let mut command = std::process::Command::new("magick");
        add_imagemagick_args(&mut command, &image_type, input, &public_path);

        tracing::info!(
            ?input,
            ?public_path,
            ?command,
            ?image_type,
            "processing photo"
        );

        let status = command.status()?;
        if !status.success() {
            return Err(anyhow::anyhow!(
                "magick failed for {} -> {}",
                input.display(),
                public_path.display()
            ));
        }
    } else {
        tracing::info!(?public_path, ?image_type, "skipping, already exists");
    }

    let (width, height) = match image_type {
        ImageType::Thumb => (500, 500),
        ImageType::Full => image_dimensions(&public_path)?,
    };

    Ok(RenderedPhoto {
        _path: public_path,
        url,
        width,
        height,
    })
}

#[tracing::instrument(err)]
fn render_html(photos: &[TemplatePhoto]) -> anyhow::Result<String> {
    tracing::info!("loading html template");
    let mut tera = Tera::default();
    tera.add_template_file("templates/index.html", Some("index.html"))?;

    let mut context = tera::Context::new();
    context.insert("photos", photos);
    context.insert("total_photos", &photos.len());

    tracing::info!(length = photos.len(), "rendering html");
    Ok(tera.render("index.html", &context)?)
}

#[tracing::instrument(err)]
fn build() -> anyhow::Result<()> {
    tracing::info!("building thumb + full photos");
    let mut seen = HashSet::new();
    let image_paths: Vec<PathBuf> = WalkDir::new(IMAGE_DIRECTORY)
        .into_iter()
        .filter_map(Result::ok)
        .filter(image_file)
        .map(|image| image.into_path())
        .collect();

    let parent = tracing::Span::current();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
    let mut photos: Vec<SourcePhoto> = pool.install(|| {
        image_paths
            .into_par_iter()
            .map(|path| {
                let _guard = parent.enter();
                let hash = hash_photo(&path)?;
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("photo")
                    .to_string();

                Ok(SourcePhoto {
                    path: path.to_path_buf(),
                    stem,
                    hash,
                })
            })
            .collect::<anyhow::Result<Vec<SourcePhoto>>>()
    })?;

    photos.retain(|photo| seen.insert(photo.hash.clone()));
    let mut processed_photos: Vec<ProcessedPhoto> = pool.install(|| {
        photos
            .into_par_iter()
            .map(|photo| {
                let _guard = parent.enter();
                let thumb = process_photo(&photo, ImageType::Thumb)?;
                let full = process_photo(&photo, ImageType::Full)?;

                Ok(ProcessedPhoto {
                    source: photo,
                    thumb,
                    full,
                })
            })
            .collect::<anyhow::Result<Vec<ProcessedPhoto>>>()
    })?;

    processed_photos.sort_by(|a, b| b.source.stem.cmp(&a.source.stem));
    tracing::debug!(?processed_photos);

    let template_photos: Vec<TemplatePhoto> = processed_photos
        .into_iter()
        .map(TemplatePhoto::from)
        .collect();

    let html = render_html(&template_photos)?;
    let html_path = PathBuf::from(PUBLIC_DIRECTORY).join("index.html");
    fs::write(html_path, html)?;

    Ok(())
}

fn init_tracing() -> anyhow::Result<trace::SdkTracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint("http://localhost:4317")
        .build()?;

    let tracer_provider = trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_service_name("domino-garden")
                .build(),
        )
        .build();

    let tracer = tracer_provider.tracer("domino-garden");

    global::set_tracer_provider(tracer_provider.clone());

    let stdout_layer = tracing_subscriber::fmt::layer().with_thread_names(true);
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or(EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(otel_layer)
        .init();

    Ok(tracer_provider)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracer_provider = init_tracing()?;
    let result = Cli::parse().command.run();

    if let Err(err) = tracer_provider.shutdown() {
        eprintln!("failed to shutdown tracer provider: {err}");
    }
    result
}
