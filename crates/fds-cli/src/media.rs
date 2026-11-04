//! Images (SPEC §3.3). FDS does not store images: it stores what a vision
//! model says about them, as text a query can find, together with the
//! image's hash and the name of the model that looked at it.
//!
//! The description is written once, when the image is added. A query never
//! calls a model. And because the text came from a model, the record says so:
//! `verified_by` is `model:<name>` until a person checks it.

use std::collections::BTreeMap;

use fds::v4::{Kind, Record};
use sha2::{Digest, Sha256};

use crate::text::{stable_id, today, word_count};

pub const IMAGE_TYPE: &str = "image";
pub const DEFAULT_VISION_MODEL: &str = "qwen2.5vl:3b";
/// A description is only as good as what the model can see; past this size
/// an image is more likely a scan to split than a photo to describe.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

const IMAGE_EXT: &[(&str, &str)] = &[
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
];

/// The media type for an image file extension, or None if FDS does not treat
/// the extension as an image.
pub fn media_type(ext: &str) -> Option<&'static str> {
    let ext = ext.to_lowercase();
    IMAGE_EXT.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

pub fn image_extensions() -> Vec<&'static str> {
    IMAGE_EXT.iter().map(|(e, _)| *e).collect()
}

/// `FDS_VISION_MODEL`, or the default.
pub fn vision_model() -> String {
    std::env::var("FDS_VISION_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VISION_MODEL.to_string())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the model is asked. Written for retrieval, not for a caption: the
/// words someone will later search with are the words on the thing, the
/// names of its parts, and where they are.
pub fn prompt(lang: Option<&str>) -> String {
    // Two parts, in this order. Asked for one description, a small model
    // summarises and drops the error code that someone will later search
    // for; asked to transcribe first, it copies the text and then describes.
    let mut p = String::from(
        "First, under the heading TEXT, copy every piece of text you can read in          this image exactly as written, one per line: codes, numbers, model names,          labels, error messages, buttons. Do not translate or correct it.          Then, under the heading DESCRIPTION, say in a short paragraph what the          image shows, naming the visible parts and where they are, so it can be          found later by a text search. Describe only what is visible; if something          cannot be made out, say so instead of guessing. No markdown.",
    );
    if let Some(name) = lang.and_then(language_name) {
        p.push_str(&format!(
            " Write the DESCRIPTION in {name}; keep the TEXT exactly as it appears."
        ));
    }
    p
}

fn language_name(code: &str) -> Option<&'static str> {
    Some(match code.get(..2)?.to_lowercase().as_str() {
        "es" => "Spanish",
        "en" => "English",
        "fr" => "French",
        "de" => "German",
        "pt" => "Portuguese",
        "it" => "Italian",
        "ja" => "Japanese",
        "zh" => "Chinese",
        _ => return None,
    })
}

/// The id an image gets: one per file name, so adding the same file again
/// replaces its description rather than adding a second one.
pub fn image_id(name: &str) -> String {
    stable_id(&format!("image:{name}"))
}

pub fn image_record(
    name: &str,
    collection: &str,
    bytes: &[u8],
    media: &str,
    description: &str,
    model: &str,
) -> Record {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name).to_string();
    let mut attrs = BTreeMap::new();
    attrs.insert("file".to_string(), name.to_string());
    attrs.insert("sha256".to_string(), sha256_hex(bytes));
    attrs.insert("bytes".to_string(), bytes.len().to_string());
    attrs.insert("media_type".to_string(), media.to_string());
    attrs.insert("described_by".to_string(), model.to_string());
    Record {
        id: image_id(name),
        kind: Kind::Knowledge,
        collection: collection.to_string(),
        question: None,
        body: description.to_string(),
        title: file,
        source: name.to_string(),
        verified_by: format!("model:{model}"),
        last_verified: today(),
        tokens: word_count(description),
        embedding: None,
        rtype: IMAGE_TYPE.to_string(),
        attrs,
        links: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_record_is_honest_about_where_its_text_came_from() {
        let r = image_record(
            "fotos/router.jpg",
            "red",
            b"not really a jpeg",
            "image/jpeg",
            "Router, back panel.",
            "qwen2.5vl:3b",
        );
        assert_eq!((r.kind, r.rtype.as_str()), (Kind::Knowledge, IMAGE_TYPE));
        assert_eq!(
            r.verified_by, "model:qwen2.5vl:3b",
            "a model wrote it, and the record says so"
        );
        assert_eq!(r.title, "router.jpg");
        assert_eq!(r.attrs["sha256"], sha256_hex(b"not really a jpeg"));
        assert_eq!(r.id, image_id("fotos/router.jpg"));
    }

    #[test]
    fn media_types_and_prompt_language() {
        assert_eq!(media_type("JPG"), Some("image/jpeg"));
        assert_eq!(media_type("md"), None);
        assert!(prompt(Some("es-BO")).contains("Write the DESCRIPTION in Spanish"));
        assert!(
            prompt(None).starts_with("First, under the heading TEXT"),
            "transcription comes first"
        );
        assert!(!prompt(None).contains("Write the DESCRIPTION in"));
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
