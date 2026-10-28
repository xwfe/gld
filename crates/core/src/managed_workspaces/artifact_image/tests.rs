use super::*;
use image::{DynamicImage, RgbImage};

fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(
        width,
        height,
        image::Rgb([12, 34, 56]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

#[test]
fn supported_formats_become_actual_png_blocks_without_textual_base64_copies() {
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Gif,
        ImageFormat::WebP,
    ] {
        let bytes = encoded(12, 8, format);
        let (metadata, block) = render(&bytes).unwrap();
        assert_eq!(block["type"], "image");
        assert_eq!(block["mimeType"], "image/png");
        let decoded = STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
        let image = image::load_from_memory(&decoded).unwrap();
        assert_eq!((image.width(), image.height()), (12, 8));
        assert_eq!(metadata["bytes"], decoded.len());
        assert!(metadata.get("data").is_none() && metadata.get("base64").is_none());
    }
}

#[test]
fn previews_shrink_dimensions_and_encoded_size_without_mutating_original_bytes() {
    let bytes = encoded(2400, 1200, ImageFormat::Png);
    let (metadata, _) = render(&bytes).unwrap();
    assert_eq!(
        (metadata["width"].as_u64(), metadata["height"].as_u64()),
        (Some(2000), Some(1000))
    );
    assert_eq!(metadata["resized"], true);
    assert_eq!(
        reader(&bytes).unwrap().into_dimensions().unwrap(),
        (2400, 1200)
    );
    let mut image = RgbImage::new(1024, 1024);
    let mut state = 0x1234_5678_u32;
    for byte in image.as_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    assert!(bytes.get_ref().len() > MAX_OUTPUT_BYTES);
    let (metadata, block) = render(bytes.get_ref()).unwrap();
    assert!(metadata["bytes"].as_u64().unwrap() <= MAX_OUTPUT_BYTES as u64);
    assert!(
        STANDARD
            .decode(block["data"].as_str().unwrap())
            .unwrap()
            .len()
            <= MAX_OUTPUT_BYTES
    );
}

#[test]
fn invalid_or_extreme_images_are_rejected_before_full_decode() {
    assert_eq!(
        render(b"not an image.png").unwrap_err().code(),
        "ARTIFACT_IMAGE_INVALID"
    );
    let bytes = encoded(MAX_DIMENSION + 1, 1, ImageFormat::Png);
    assert_eq!(
        render(&bytes).unwrap_err().code(),
        "ARTIFACT_IMAGE_TOO_LARGE"
    );
    assert_eq!(
        render(&vec![0; MAX_INPUT_BYTES as usize + 1])
            .unwrap_err()
            .code(),
        "ARTIFACT_IMAGE_TOO_LARGE"
    );
}

#[test]
fn image_artifacts_remain_verified_after_closure_and_tampering_is_not_rendered() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "image", "").unwrap();
    // 文件名不是类型依据，实际 PNG 必须仍能识别。
    let bytes = encoded(12, 8, ImageFormat::Png);
    fs::write(record.root.join("preview.txt"), &bytes).unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    let receipt = op.finish(&["preview.txt".into()], None).unwrap();
    let id = receipt["artifacts"][0]["id"].as_str().unwrap();
    let (metadata, image) = op.artifact_image(id).unwrap();
    assert_eq!(metadata["verified"], true);
    assert_eq!(metadata["image"]["original"]["mime_type"], "image/png");
    assert_eq!(image["type"], "image");
    assert!(!record.root.exists());
    let saved = home.path().join("artifacts").join(&record.id).join(id);
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    fs::write(saved, b"tampered").unwrap();
    assert_eq!(
        op.artifact_image(id).unwrap_err().code(),
        "ARTIFACT_INTEGRITY"
    );
}
