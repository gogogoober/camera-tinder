//! EXIF metadata: capture time and camera.

use std::io::{BufRead, Seek};

use chrono::{NaiveDate, NaiveDateTime};
use exif::{In, Tag, Value};

#[derive(Debug, Clone, Default)]
pub struct PhotoMeta {
    pub captured_at: Option<NaiveDateTime>,
    pub camera: Option<String>,
    /// EXIF Make as written by the camera ("NIKON CORPORATION", "FUJIFILM").
    pub make: Option<String>,
}

pub fn read_meta<R: BufRead + Seek>(r: &mut R) -> PhotoMeta {
    let Ok(exif) = exif::Reader::new().read_from_container(r) else {
        return PhotoMeta::default();
    };

    let ascii = |tag: Tag| -> Option<String> {
        let field = exif.get_field(tag, In::PRIMARY)?;
        match &field.value {
            Value::Ascii(v) => v.first().map(|b| {
                String::from_utf8_lossy(b).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
            }),
            _ => None,
        }
        .filter(|s| !s.is_empty())
    };

    let captured_at = [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime]
        .into_iter()
        .find_map(|tag| {
            let field = exif.get_field(tag, In::PRIMARY)?;
            let Value::Ascii(v) = &field.value else { return None };
            let dt = exif::DateTime::from_ascii(v.first()?).ok()?;
            NaiveDate::from_ymd_opt(dt.year.into(), dt.month.into(), dt.day.into())?.and_hms_opt(
                dt.hour.into(),
                dt.minute.into(),
                dt.second.into(),
            )
        });

    let make = ascii(Tag::Make);
    let camera = match (make.clone(), ascii(Tag::Model)) {
        // Many models already include the make ("NIKON Z 6_2"), avoid "NIKON CORPORATION NIKON Z 6_2".
        (Some(make), Some(model)) => {
            let brand = make.split_whitespace().next().unwrap_or(&make).to_string();
            if model.to_lowercase().starts_with(&brand.to_lowercase()) { Some(model) } else { Some(format!("{brand} {model}")) }
        }
        (None, Some(model)) => Some(model),
        (Some(make), None) => Some(make),
        (None, None) => None,
    };

    PhotoMeta { captured_at, camera, make }
}
