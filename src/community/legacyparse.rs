use std::collections::HashMap;

use crate::community::models::common::{
    ManifestAuthorV2, ManifestDownloadUpdateLogV2, ManifestDownloadV2, ManifestItemV2,
    ManifestLinkV2, ManifestV2,
};

fn map_download_key_v1_to_v2(key: &str) -> String {
    // 不需要再维护这个列表了，v1的设备支持到s5和rw6即为终点
    let ret = match key {
        // Xiaomi Watch S3 系列
        "n62" => "xmws3",

        // Xiaomi Watch S4 系列
        "o62" => "xmws4",
        "o62m" => "xmws4xring",

        // Xiaomi Watch S5 系列
        "p62" => "xmws5",
        "p62m" => "xmws5xring",

        // REDMI Watch 5
        "o65" => "xmrw5",
        "o65m" => "xmrw5xring",

        // Band 系列
        "n66" => "xmb9",
        "n67" => "xmb9p",
        "o66" => "xmb10",
        "o66nfc" => "xmb10nfc",

        // REDMI Watch 6
        "p65" => "xmrw6",

        other => other,
    };

    ret.to_string()
}

pub fn manifest_v1_to_v2(raw: serde_json::Value) -> anyhow::Result<ManifestV2> {
    let item = raw
        .get("item")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

    let downloads = raw
        .get("downloads")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

    let links_value = raw
        .get("links")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(vec![]));

    let ext = raw.get("ext").cloned().unwrap_or(serde_json::Value::Null);

    let mut item_v2 = ManifestItemV2::default();

    item_v2.id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    item_v2.name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    item_v2.description = item
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    item_v2.preview = item
        .get("preview")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    item_v2.icon = item
        .get("icon")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    item_v2.cover = item
        .get("cover")
        .and_then(|v| v.as_str())
        .unwrap_or(item_v2.icon.as_str())
        .to_string();

    if let Some(arr) = item.get("author").and_then(|v| v.as_array()) {
        item_v2.author = arr
            .iter()
            .map(|a| {
                let name = a
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let bind_ab_account = a
                    .get("bindABAccount")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                ManifestAuthorV2 {
                    name,
                    bind_ab_account,
                }
            })
            .collect();
    }

    let mut links_v2 = Vec::new();
    if let Some(arr) = links_value.as_array() {
        for l in arr {
            let title = l
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let url = l
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if title.is_empty() && url.is_empty() {
                continue;
            }

            let icon = l
                .get("icon")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            links_v2.push(ManifestLinkV2 { icon, title, url });
        }
    }

    let mut downloads_v2: HashMap<String, ManifestDownloadV2> = HashMap::new();
    if let Some(obj) = downloads.as_object() {
        for (k, v) in obj {
            let mapped_key = map_download_key_v1_to_v2(k);

            let version = v
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let file_name = v
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let url = v.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
            let sha256 = v
                .get("sha256")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let display_name = v
                .get("display_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut updatelogs: Option<Vec<ManifestDownloadUpdateLogV2>> = None;
            if let Some(arr) = v.get("updatelogs").and_then(|v| v.as_array()) {
                let mut logs = Vec::new();
                for log in arr {
                    if let (Some(ver), Some(content)) = (
                        log.get("version").and_then(|v| v.as_str()),
                        log.get("content").and_then(|v| v.as_str()),
                    ) {
                        logs.push(ManifestDownloadUpdateLogV2 {
                            version: ver.to_string(),
                            content: content.to_string(),
                        });
                    }
                }
                if !logs.is_empty() {
                    updatelogs = Some(logs);
                }
            }

            downloads_v2.insert(
                mapped_key,
                ManifestDownloadV2 {
                    version,
                    file_name,
                    url,
                    sha256,
                    display_name,
                    updatelogs,
                },
            );
        }
    }

    Ok(ManifestV2 {
        item: item_v2,
        links: links_v2,
        downloads: downloads_v2,
        ext,
    })
}
