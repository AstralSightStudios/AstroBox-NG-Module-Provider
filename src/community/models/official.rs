use serde::{Deserialize, Deserializer, Serialize};

fn split_semicolon<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    Ok(s.split(';').map(|x| x.trim().to_string()).collect())
}

// V2 规范: https://affine.astralsight.space/workspace/af61c26a-3d53-46ca-85e7-89772913da6d/VVn-o4ALtyuf6NbdenmjJ
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexV2 {
    pub id: String,               // 资源唯一ID
    pub name: String,             // 资源名称
    pub restype: ResourceTypeV2,  // 资源类型
    pub repo_owner: String,       // 资源仓库拥有者 (Github Only)
    pub repo_name: String,        // 资源仓库名称 (Github Only)
    pub repo_commit_hash: String, // 资源仓库提交哈希
    pub icon: String,             // 资源图标路径
    pub cover: String,            // 资源封面路径
    #[serde(deserialize_with = "split_semicolon")]
    pub tags: Vec<String>, // 资源标签
    #[serde(deserialize_with = "split_semicolon")]
    pub device_vendors: Vec<String>, // 资源支持的设备厂商
    #[serde(deserialize_with = "split_semicolon")]
    pub devices: Vec<String>, // 资源支持的设备型号
    pub paid_type: PaidTypeV2,    // 资源付费类型
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ResourceTypeV2 {
    #[serde(rename = "quickapp")]
    QuickApp, // 快应用
    #[serde(rename = "watchface")]
    WatchFace, // 表盘
    #[serde(rename = "firmware")]
    Firmware, // 固件
    #[serde(rename = "fontpack")]
    FontPack, // 字体包
    #[serde(rename = "iconpack")]
    IconPack, // 图标包
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PaidTypeV2 {
    #[serde(rename = "")]
    Free, // 免费
    #[serde(rename = "paid")]
    Paid, // 付费（内含付费内容）
    #[serde(rename = "force_paid")]
    ForcePaid, // 强制付费（不给钱不让用）
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct DeviceMapV2 {
    pub xiaomi: Vec<DeviceV2>,
    pub vivo: Vec<DeviceV2>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceV2 {
    pub id: String,
    pub name: String,
    pub description: String,
    pub chip: DeviceChipV2,
    pub fetch: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum DeviceChipV2 {
    #[serde(rename = "xring")]
    XRing,
    #[serde(rename = "bes")]
    Bes,
}
