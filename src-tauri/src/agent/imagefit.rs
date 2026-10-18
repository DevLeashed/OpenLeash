//! Keep every image sent to a model within the pixel size providers accept.
//!
//! Anthropic rejects the *whole request* with a 400 when any image has a side
//! over 8000 px ("At least one of the image dimensions exceed max allowed size"),
//! and because the image stays in history every later turn fails the same way:
//! one tall full-page screenshot or pasted panorama bricks the chat. Byte size
//! is already capped elsewhere (`tools::MAX_IMAGE_BYTES`); this is about pixels.
//!
//! Images are fitted where they enter (`view_image`, pasted photos, images handed
//! to a subagent) and again on every outgoing request, so history saved before
//! this existed — or an image arriving by a path that does not fit it (MCP, hooks)
//! — cannot wedge a chat either.
//!
//! Only PNG can be re-encoded: the `image` crate is built with the PNG codec
//! alone. An oversized JPEG/GIF/WebP is refused at entry with a message saying
//! what to do, and in history it becomes a text stub rather than a failed request.

use super::Message;
use base64::Engine as _;
use serde_json::{json, Value};

/// The largest side any provider we send to accepts (Anthropic's hard limit).
pub const MAX_SIDE: u32 = 8000;

/// What an oversized image is shrunk to (longest edge). Anthropic downsamples
/// anything over ~1568 px before the model sees it, and OpenAI to 2048, so a
/// smaller target loses nothing the model would have seen, keeps the request
/// small, and stays under Anthropic's 2000 px cap for requests with many images.
pub const FIT_EDGE: u32 = 2000;

/// Pixel size from the image header, without decoding the image.
/// `None` for anything unrecognised or truncated.
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le24 = |i: usize| {
        Some(u32::from_le_bytes([
            *b.get(i)?,
            *b.get(i + 1)?,
            *b.get(i + 2)?,
            0,
        ]))
    };
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is always the first chunk.
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.len() >= 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8L" if b[20] == 0x2f => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8 " if b[23..26] == [0x9d, 0x01, 0x2a] => {
                Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        // Walk the segments to the first frame header (SOFn).
        let mut i = 2;
        while i + 3 < b.len() {
            if b[i] != 0xFF {
                return None;
            }
            let m = b[i + 1];
            if m == 0xFF {
                i += 1; // fill byte
                continue;
            }
            if m == 0x01 || (0xD0..=0xD8).contains(&m) {
                i += 2; // standalone marker, no length
                continue;
            }
            if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            if m == 0xDA || m == 0xD9 {
                return None; // scan data before any frame header
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

/// Whether a `width x height` image is over what a provider accepts.
pub fn too_big((w, h): (u32, u32)) -> bool {
    w > MAX_SIDE || h > MAX_SIDE
}

/// Shrink a PNG so its longest edge is `FIT_EDGE`. `None` if it can't be decoded.
fn shrink_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let small = img.resize(FIT_EDGE, FIT_EDGE, image::imageops::FilterType::Triangle);
    let mut buf = std::io::Cursor::new(Vec::new());
    small.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(buf.into_inner())
}

fn refusal(display: &str, (w, h): (u32, u32), media_type: &str) -> String {
    format!(
        "{display} is {w}x{h} px; models accept at most {MAX_SIDE} px per side, and only PNG images can be shrunk automatically ({media_type} can't). Resize it to at most {FIT_EDGE} px on its longest side (or save it as PNG) and try again."
    )
}

/// Fit raw image bytes for sending. Returns the (possibly new) media type and
/// bytes; images within the limit, or whose size can't be read, pass through
/// untouched. `display` names the image in the error.
pub fn fit_bytes(
    display: &str,
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<(String, Vec<u8>), String> {
    let Some(dims) = dimensions(&bytes).filter(|d| too_big(*d)) else {
        return Ok((media_type.to_string(), bytes));
    };
    if media_type != "image/png" {
        return Err(refusal(display, dims, media_type));
    }
    match shrink_png(&bytes) {
        Some(small) => Ok(("image/png".to_string(), small)),
        None => Err(format!(
            "{display} is {}x{} px, over the {MAX_SIDE} px per side models accept, and it couldn't be decoded to shrink it.",
            dims.0, dims.1
        )),
    }
}

/// Header-only size of a base64 image. Decodes a small prefix first; JPEG
/// headers can sit behind large EXIF blocks, so it widens only when needed.
fn b64_dimensions(data: &str) -> Option<(u32, u32)> {
    let eng = &base64::engine::general_purpose::STANDARD;
    for take in [64usize, 96 * 1024] {
        if data.len() <= take {
            break;
        }
        let prefix = &data.as_bytes()[..take];
        if let Some(d) = eng.decode(prefix).ok().and_then(|b| dimensions(&b)) {
            return Some(d);
        }
    }
    dimensions(&eng.decode(data).ok()?)
}

/// What to do with one base64 image: `None` = fine as it is; `Some(Ok)` = the
/// replacement (media type, base64); `Some(Err)` = it can't be fitted.
pub fn fit_b64(
    display: &str,
    media_type: &str,
    data: &str,
) -> Option<Result<(String, String), String>> {
    let dims = b64_dimensions(data)?;
    if !too_big(dims) {
        return None;
    }
    let eng = &base64::engine::general_purpose::STANDARD;
    let Ok(bytes) = eng.decode(data) else {
        return Some(Err(refusal(display, dims, media_type)));
    };
    Some(fit_bytes(display, media_type, bytes).map(|(mt, b)| (mt, eng.encode(b))))
}

/// Fit one block in place if it is an oversized image. Returns whether it changed.
fn fit_block(b: &mut Value) -> bool {
    if b["type"] != "image" {
        return false;
    }
    let (Some(mt), Some(data)) = (
        b["source"]["media_type"].as_str(),
        b["source"]["data"].as_str(),
    ) else {
        return false;
    };
    match fit_b64("An image", mt, data) {
        None => false,
        Some(Ok((mt, data))) => {
            b["source"]["media_type"] = json!(mt);
            b["source"]["data"] = json!(data);
            true
        }
        // Dropping the picture is the only way the rest of the chat can still be
        // sent; the stub tells the model something was here and why it's gone.
        Some(Err(e)) => {
            *b = json!({"type": "text", "text": format!("[image removed: {e}]")});
            true
        }
    }
}

/// Whether any image in a history needs `fit_messages`, without changing it,
/// so a caller holding a borrowed history copies it only when it must.
pub fn needs_fit(msgs: &[Message]) -> bool {
    let over = |b: &Value| {
        b["type"] == "image"
            && b["source"]["data"]
                .as_str()
                .and_then(b64_dimensions)
                .is_some_and(too_big)
    };
    msgs.iter().flat_map(|m| m.content.iter()).any(|b| {
        over(b)
            || b["content"]
                .as_array()
                .is_some_and(|a| b["type"] == "tool_result" && a.iter().any(over))
    })
}

/// Fit every image in a history: user images and those inside tool results.
/// Returns whether anything changed. Cheap when nothing is oversized: only each
/// image's header is decoded.
pub fn fit_messages(msgs: &mut [Message]) -> bool {
    let mut changed = false;
    for m in msgs.iter_mut() {
        for b in m.content.iter_mut() {
            changed |= fit_block(b);
            if b["type"] == "tool_result" {
                if let Some(a) = b["content"].as_array_mut() {
                    for c in a.iter_mut() {
                        changed |= fit_block(c);
                    }
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    /// A JPEG header with an APP0 segment before the frame header, as real files
    /// have; only the header matters to `dimensions`.
    fn jpeg_header(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn reads_sizes_from_headers() {
        assert_eq!(dimensions(&png(7, 3)), Some((7, 3)));
        assert_eq!(dimensions(&jpeg_header(9000, 120)), Some((9000, 120)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x1f, 0x0a, 0x00]); // 8000 x 10
        assert_eq!(dimensions(&gif), Some((8000, 10)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend_from_slice(&[0; 8]);
        webp.extend_from_slice(&[0x40, 0x1f, 0x00, 0x09, 0x00, 0x00]); // 8001 x 10
        assert_eq!(dimensions(&webp), Some((8001, 10)));
        assert_eq!(dimensions(b"not an image at all"), None);
    }

    #[test]
    fn an_oversized_png_is_shrunk_and_a_normal_one_left_alone() {
        let small = png(40, 30);
        let (mt, out) = fit_bytes("s.png", "image/png", small.clone()).unwrap();
        assert_eq!((mt.as_str(), out), ("image/png", small));

        let tall = png(10, MAX_SIDE + 1);
        let (mt, out) = fit_bytes("t.png", "image/png", tall).unwrap();
        assert_eq!(mt, "image/png");
        let (w, h) = dimensions(&out).unwrap();
        assert!(!too_big((w, h)));
        assert_eq!(h, FIT_EDGE, "the longest edge lands on the target");
    }

    #[test]
    fn an_oversized_non_png_is_refused_with_a_reason() {
        let e = fit_bytes("pano.jpg", "image/jpeg", jpeg_header(12000, 900)).unwrap_err();
        assert!(e.contains("12000x900") && e.contains("pano.jpg"), "{e}");
        // Within the limit it passes untouched, whatever the format.
        assert!(fit_bytes("ok.jpg", "image/jpeg", jpeg_header(4000, 3000)).is_ok());
    }

    #[test]
    fn history_with_an_oversized_image_becomes_sendable() {
        let tall = b64(&png(12, MAX_SIDE + 500));
        let fine = b64(&png(5, 5));
        let wide_jpeg = b64(&jpeg_header(9001, 50));
        let img = |mt: &str, d: &str| json!({"type": "image", "source": {"type": "base64", "media_type": mt, "data": d}});
        let mut msgs = vec![
            Message::user(vec![img("image/png", &tall), img("image/png", &fine)]),
            Message::user(vec![json!({
                "type": "tool_result", "tool_use_id": "x",
                "content": [{"type": "text", "text": "Viewed"}, img("image/jpeg", &wide_jpeg)],
            })]),
        ];
        assert!(needs_fit(&msgs));
        assert!(fit_messages(&mut msgs));
        assert!(!needs_fit(&msgs));

        let first = &msgs[0].content;
        let data = first[0]["source"]["data"].as_str().unwrap();
        assert!(!too_big(b64_dimensions(data).unwrap()));
        assert_eq!(
            first[1]["source"]["data"],
            json!(fine),
            "a fine image is untouched"
        );
        let stub = &msgs[1].content[0]["content"][1];
        assert_eq!(stub["type"], "text");
        assert!(stub["text"].as_str().unwrap().contains("9001x50"));

        assert!(
            !fit_messages(&mut msgs),
            "a second pass finds nothing to do"
        );
    }
}
