use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use walkdir::{DirEntry, WalkDir};

const IMAGE_DIRECTORY: &str = "originals/";
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
    path: PathBuf,
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
    fn run(&self) -> anyhow::Result<()> {
        match self {
            CliCommand::Build {} => build(),
            CliCommand::Deploy {} => todo!(),
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

fn image_dimension(path: &Path) -> anyhow::Result<(u32, u32)> {
    let output = Command::new("magick")
        .arg("identify")
        .arg("-format")
        .arg("%w %h")
        .arg(path)
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

fn generate_photo(photo: &SourcePhoto, image_type: ImageType) -> anyhow::Result<RenderedPhoto> {
    let stem = &photo.stem;
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
            "generating photo"
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
        ImageType::Full => image_dimension(&public_path)?,
    };

    Ok(RenderedPhoto {
        path: public_path,
        url,
        width,
        height,
    })
}

fn build() -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    let photos: Vec<SourcePhoto> = WalkDir::new(IMAGE_DIRECTORY)
        .into_iter()
        .filter_map(Result::ok)
        .filter(image_file)
        .filter_map(|image| {
            let path = image.into_path();
            let hash = hash_photo(&path).ok()?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("photo")
                .to_string();

            Some(SourcePhoto { path, stem, hash })
        })
        .filter(|photo| seen.insert(photo.hash.clone()))
        .collect();

    let mut processed_photos: Vec<ProcessedPhoto> = vec![];

    for photo in photos {
        let thumb = generate_photo(&photo, ImageType::Thumb)?;
        let full = generate_photo(&photo, ImageType::Full)?;
        processed_photos.push(ProcessedPhoto {
            source: photo,
            thumb,
            full,
        })
    }

    tracing::debug!(?processed_photos);

    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_thread_names(true).init();
    Cli::parse().command.run()
}
