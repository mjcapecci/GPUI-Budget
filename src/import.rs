//! Reading transactions out of screenshots with Claude's vision API.
//!
//! [`extract`] sends one screenshot to the Messages API and asks for the
//! transactions in it as JSON matching a schema, along with a suggested
//! category for each. It blocks for the length of the request, so call it off
//! the UI thread. Nothing here touches the database; the import screen turns
//! the results into transactions after the user reviews them.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use chrono::NaiveDate;
use image::{ImageFormat, ImageReader, imageops::FilterType};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{Category, CategoryKind, Money};

/// Settings key holding the Anthropic API key.
pub const API_KEY_SETTING: &str = "anthropic_api_key";
/// Used when no key is saved in Settings.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-5-5";
/// Lets the API retry a request on another model if a safety classifier
/// declines it, instead of failing the import.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// The largest image the model reads at full detail; bigger ones are
/// downscaled by the API anyway, so shrink them before uploading.
const MAX_EDGE: u32 = 2576;
const MAX_PIXELS: u32 = 3_750_000;
/// The API rejects images over 5 MB once base64 encoded (4/3 larger).
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024 * 3 / 4;

/// One transaction read from a screenshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    pub date: NaiveDate,
    /// Always positive.
    pub amount: Money,
    /// A cleaned-up merchant or payee name, used as the note.
    pub merchant: String,
    /// Money coming in, such as a refund, deposit or card payment.
    pub credit: bool,
    /// Marked pending on screen, so the amount may still change.
    pub pending: bool,
    /// Claude's suggestion, always one of the categories it was given.
    pub category_id: Option<i64>,
}

/// Read the transactions in a screenshot. `today` resolves dates like
/// "Yesterday" or "Oct 3" that have no year.
pub fn extract(
    api_key: &str,
    image: &[u8],
    categories: &[Category],
    today: NaiveDate,
) -> Result<Vec<Extracted>> {
    let (media_type, image) = prepare_image(image)?;
    let body = request_body(media_type, &image, categories, today);
    let response = send(api_key, &body)?;
    parse_response(&response, categories)
}

/// Check the image is one the API accepts, shrinking it if it is larger
/// than the model can use. Returns the media type and bytes to upload.
fn prepare_image(bytes: &[u8]) -> Result<(&'static str, Vec<u8>)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("reading the image")?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow!("Unsupported image type. Use a PNG, JPEG or WebP screenshot."))?;
    let media_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => bail!("Unsupported image type. Use a PNG, JPEG or WebP screenshot."),
    };
    let (width, height) = reader
        .into_dimensions()
        .context("Couldn't read this image. Is the file a screenshot?")?;
    if width.max(height) <= MAX_EDGE
        && width.saturating_mul(height) <= MAX_PIXELS
        && bytes.len() <= MAX_IMAGE_BYTES
    {
        return Ok((media_type, bytes.to_vec()));
    }

    let image = image::load_from_memory_with_format(bytes, format)
        .context("Couldn't read this image. Is the file a screenshot?")?;
    let scale = (MAX_EDGE as f64 / width.max(height) as f64)
        .min((MAX_PIXELS as f64 / (width as f64 * height as f64)).sqrt())
        .min(1.0);
    let image = if scale < 1.0 {
        let w = ((width as f64 * scale) as u32).max(1);
        let h = ((height as f64 * scale) as u32).max(1);
        image.resize_exact(w, h, FilterType::Lanczos3)
    } else {
        image
    };

    // PNG keeps text crisp; fall back to JPEG if it is still too large.
    let mut png = Vec::new();
    image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    if png.len() <= MAX_IMAGE_BYTES {
        return Ok(("image/png", png));
    }
    let mut jpeg = Vec::new();
    image
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)?;
    if jpeg.len() > MAX_IMAGE_BYTES {
        bail!("This image is too large to upload. Try a smaller screenshot.");
    }
    Ok(("image/jpeg", jpeg))
}

const SYSTEM_PROMPT: &str = "\
You read screenshots of bank, credit card and payment app transaction lists \
for a personal budgeting app, and return every transaction visible in the \
screenshot.

- date: the date of the transaction. Resolve relative dates (\"Today\", \
\"Yesterday\", a weekday) and dates without a year to the most recent such \
date on or before today. If rows are grouped under date headings, use the \
heading above the row.
- amount: the absolute amount as a plain decimal, like \"1234.50\", with no \
currency symbol, sign or thousands separator.
- credit: true for money coming in (refunds, deposits, payments toward a card \
balance, transfers in), false for purchases and other money going out. Judge \
from the sign, color and wording shown.
- merchant: a short, readable merchant or payee name, like \"Trader Joe's\" \
rather than \"TRADER JOE'S #552 SEATTLE WA\".
- pending: true if the row is marked pending or authorized.
- category_id: the id of the category that best fits, choosing an income \
category only for income such as pay or deposits, or null if none fits well.

Skip balances, totals, headings and rows cut off so that the amount or \
merchant can't be read. Never guess at a transaction that isn't shown. If the \
image isn't a list of transactions, return an empty list.";

fn request_body(
    media_type: &str,
    image: &[u8],
    categories: &[Category],
    today: NaiveDate,
) -> Value {
    let category_list: String = categories
        .iter()
        .map(|c| format!("{}: {} ({})\n", c.id, c.name, c.kind.as_str()))
        .collect();
    let prompt = format!(
        "Today is {}.\n\nCategories (id: name (kind)):\n{category_list}",
        today.format("%A, %B %-d, %Y")
    );
    json!({
        "model": MODEL,
        "max_tokens": 16000,
        "fallbacks": "default",
        "output_config": {
            "effort": "medium",
            "format": {"type": "json_schema", "schema": schema(categories)},
        },
        "system": SYSTEM_PROMPT,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": base64::engine::general_purpose::STANDARD.encode(image),
                    },
                },
                {"type": "text", "text": prompt},
            ],
        }],
    })
}

/// The JSON the response must match. `category_id` is limited to the ids of
/// the given categories.
fn schema(categories: &[Category]) -> Value {
    let ids: Vec<i64> = categories.iter().map(|c| c.id).collect();
    let category_id = if ids.is_empty() {
        json!({"type": "null"})
    } else {
        json!({"anyOf": [{"type": "integer", "enum": ids}, {"type": "null"}]})
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["transactions"],
        "properties": {
            "transactions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["date", "amount", "merchant", "credit", "pending", "category_id"],
                    "properties": {
                        "date": {"type": "string", "format": "date"},
                        "amount": {"type": "string"},
                        "merchant": {"type": "string"},
                        "credit": {"type": "boolean"},
                        "pending": {"type": "boolean"},
                        "category_id": category_id,
                    },
                },
            },
        },
    })
}

/// POST the request, retrying a couple of times while the API is busy.
fn send(api_key: &str, body: &Value) -> Result<Value> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(180)))
        .build()
        .new_agent();
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut response = agent
            .post(API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", FALLBACK_BETA)
            .send_json(body)
            .map_err(|e| anyhow!("Couldn't reach Anthropic's API: {e}"))?;
        let status = response.status().as_u16();
        let json: Value = response
            .body_mut()
            .with_config()
            .limit(10 * 1024 * 1024)
            .read_json()
            .with_context(|| format!("reading the API response (HTTP {status})"))?;
        if status == 200 {
            return Ok(json);
        }
        let retryable = status == 429 || status >= 500;
        if retryable && attempt < 3 {
            std::thread::sleep(Duration::from_secs(2 * attempt));
            continue;
        }
        let message = json["error"]["message"].as_str().unwrap_or("unknown error");
        bail!(match status {
            401 => "Anthropic rejected the API key. Check it in Settings › Import.".to_string(),
            429 =>
                format!("Anthropic's API is rate limiting requests. Try again shortly. ({message})"),
            529 => "Anthropic's API is overloaded. Try again shortly.".to_string(),
            _ => format!("Anthropic's API returned an error (HTTP {status}): {message}"),
        });
    }
}

#[derive(Deserialize)]
struct Output {
    transactions: Vec<OutputRow>,
}

#[derive(Deserialize)]
struct OutputRow {
    date: String,
    amount: String,
    merchant: String,
    credit: bool,
    pending: bool,
    category_id: Option<i64>,
}

/// Turn a Messages API response into transactions. Rows whose date or amount
/// can't be parsed are dropped, and suggested categories that don't exist,
/// or are income for money going out, are cleared.
fn parse_response(response: &Value, categories: &[Category]) -> Result<Vec<Extracted>> {
    match response["stop_reason"].as_str() {
        Some("refusal") => bail!("Claude declined to read this screenshot."),
        Some("max_tokens") => {
            bail!("This screenshot has too many transactions to read at once. Try a smaller one.")
        }
        _ => {}
    }
    let text = response["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<String>();
    let output: Output =
        serde_json::from_str(&text).context("Claude's reply wasn't in the expected format")?;

    Ok(output
        .transactions
        .into_iter()
        .filter_map(|row| {
            let date = NaiveDate::parse_from_str(&row.date, "%Y-%m-%d").ok()?;
            let amount = Money::parse(&row.amount).map(|m| Money(m.0.abs()))?;
            if amount.0 == 0 {
                return None;
            }
            let category_id = row.category_id.filter(|id| {
                categories
                    .iter()
                    .any(|c| c.id == *id && (row.credit || c.kind == CategoryKind::Expense))
            });
            Some(Extracted {
                date,
                amount,
                merchant: row.merchant.trim().to_string(),
                credit: row.credit,
                pending: row.pending,
                category_id,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn categories() -> Vec<Category> {
        vec![
            Category {
                id: 1,
                name: "Salary".into(),
                kind: CategoryKind::Income,
            },
            Category {
                id: 4,
                name: "Groceries".into(),
                kind: CategoryKind::Expense,
            },
        ]
    }

    fn response(stop_reason: &str, text: &str) -> Value {
        json!({
            "stop_reason": stop_reason,
            "content": [
                {"type": "thinking", "thinking": "", "signature": "x"},
                {"type": "text", "text": text},
            ],
        })
    }

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn parses_transactions_and_checks_categories() {
        let text = json!({"transactions": [
            {"date": "2026-10-09", "amount": "$1,234.50", "merchant": " Trader Joe's ",
             "credit": false, "pending": true, "category_id": 4},
            // Income category for a purchase: cleared.
            {"date": "2026-10-08", "amount": "-12", "merchant": "Shell",
             "credit": false, "pending": false, "category_id": 1},
            // Unknown category: cleared.
            {"date": "2026-10-07", "amount": "3.00", "merchant": "Payroll",
             "credit": true, "pending": false, "category_id": 99},
            // Unreadable rows are dropped.
            {"date": "Oct 7", "amount": "3.00", "merchant": "x",
             "credit": false, "pending": false, "category_id": null},
            {"date": "2026-10-07", "amount": "0", "merchant": "x",
             "credit": false, "pending": false, "category_id": null},
        ]})
        .to_string();
        let rows = parse_response(&response("end_turn", &text), &categories()).unwrap();
        assert_eq!(
            rows,
            [
                Extracted {
                    date: date("2026-10-09"),
                    amount: Money(123450),
                    merchant: "Trader Joe's".into(),
                    credit: false,
                    pending: true,
                    category_id: Some(4),
                },
                Extracted {
                    date: date("2026-10-08"),
                    amount: Money(1200),
                    merchant: "Shell".into(),
                    credit: false,
                    pending: false,
                    category_id: None,
                },
                Extracted {
                    date: date("2026-10-07"),
                    amount: Money(300),
                    merchant: "Payroll".into(),
                    credit: true,
                    pending: false,
                    category_id: None,
                },
            ]
        );
    }

    #[test]
    fn reports_refusals_and_truncation() {
        let err = parse_response(&response("refusal", ""), &categories()).unwrap_err();
        assert!(err.to_string().contains("declined"));
        let err = parse_response(&response("max_tokens", "{\"tr"), &categories()).unwrap_err();
        assert!(err.to_string().contains("too many"));
    }

    #[test]
    fn request_lists_categories_and_limits_ids() {
        let body = request_body("image/png", b"png", &categories(), date("2026-10-11"));
        assert_eq!(body["model"], MODEL);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["source"]["data"], "cG5n");
        let text = content[1]["text"].as_str().unwrap();
        assert!(text.contains("Sunday, October 11, 2026"));
        assert!(text.contains("4: Groceries (expense)"));
        let ids = &body["output_config"]["format"]["schema"]["properties"]["transactions"]["items"]
            ["properties"]["category_id"]["anyOf"][0]["enum"];
        assert_eq!(ids, &json!([1, 4]));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn small_images_are_sent_as_is_and_large_ones_shrunk() {
        let small = png(1179, 2556);
        let (media_type, bytes) = prepare_image(&small).unwrap();
        assert_eq!((media_type, bytes.len()), ("image/png", small.len()));

        let (media_type, bytes) = prepare_image(&png(1200, 5200)).unwrap();
        assert_eq!(media_type, "image/png");
        let shrunk = image::load_from_memory(&bytes).unwrap();
        assert!(shrunk.height() <= MAX_EDGE && shrunk.height() > MAX_EDGE - 2);
        assert!(shrunk.width() * shrunk.height() <= MAX_PIXELS);

        assert!(prepare_image(b"not an image").is_err());
    }
}
