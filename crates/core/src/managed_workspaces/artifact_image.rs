//! 已验证产物字节的有界预览；使用现有 image/base64 依赖，不回写原件或创建预览缓存。

use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageFormat, ImageReader, Limits};
use serde_json::{json, Value};
use std::io::Cursor;

pub(super) const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 40_000_000;

fn reader(data: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_| invalid_image())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    // image 的 max_alloc 是尽力限制，不把它描述为整个进程的硬内存配额。
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader)
}

pub(super) fn render(data: &[u8]) -> Result<(Value, Value)> {
    if data.len() as u64 > MAX_INPUT_BYTES {
        return Err(too_large());
    }
    let header = reader(data)?;
    let original_mime = match header.format() {
        Some(ImageFormat::Png) => "image/png",
        Some(ImageFormat::Jpeg) => "image/jpeg",
        Some(ImageFormat::Gif) => "image/gif",
        Some(ImageFormat::WebP) => "image/webp",
        _ => return Err(invalid_image()),
    };
    let (width, height) = header.into_dimensions().map_err(decode_error)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(too_large());
    }
    let image = reader(data)?.decode().map_err(decode_error)?;
    // thumbnail 也会放大小图；预览只缩小，不能把小图放大增加体积并改变原始尺寸。
    let mut edge = width.max(height).min(2000);
    for _ in 0..6 {
        let preview = image.thumbnail(edge, edge);
        let (shown_width, shown_height) = (preview.width(), preview.height());
        let mut encoded = Cursor::new(Vec::new());
        preview
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(decode_error)?;
        let bytes = encoded.into_inner();
        if bytes.len() <= MAX_OUTPUT_BYTES {
            return Ok((
                json!({"mime_type":"image/png","bytes":bytes.len(),"width":shown_width,"height":shown_height,
                    "resized":shown_width != width || shown_height != height,"frame":"first",
                    "original":{"bytes":data.len(),"width":width,"height":height,"mime_type":original_mime}}),
                json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(&bytes)}),
            ));
        }
        edge = (edge * 3 / 4).max(1);
    }
    Err(too_large())
}

fn invalid_image() -> WorkspaceError {
    error(
        "ARTIFACT_IMAGE_INVALID",
        "Artifact is not a valid PNG, JPEG, GIF or WebP image; query metadata instead",
        "validation",
    )
}

fn too_large() -> WorkspaceError {
    error("ARTIFACT_IMAGE_TOO_LARGE", "Preview requires input <=16 MiB, each dimension <=16384, at most 40 million pixels and output <=2 MiB; original artifact was kept", "resource")
}

fn decode_error(error: image::ImageError) -> WorkspaceError {
    match error {
        image::ImageError::Limits(_) => too_large(),
        _ => invalid_image(),
    }
}

#[cfg(test)]
mod tests;
