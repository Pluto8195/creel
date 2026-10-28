//! Device-local discovery metadata and the browser's live preview list.
use serde::{Deserialize, Serialize};

pub const PREVIEW_PROXY_PORT: u16 = 7331;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewService {
    /// Persisted identities; neither includes a listening port or process ID.
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub project_cwd: String,
    pub device_id: String,
    pub device_name: String,
    pub hostname: String,
    pub name: String,
    pub port: u16,
    pub pid: u32,
    pub cwd: String,
    /// Process creation time, milliseconds since the Unix epoch.
    pub started_at: u64,
    /// Wire name stays `zeronOwned`: the edge worker validates it and older
    /// devices read it. `mastOwned` is also accepted on read.
    #[serde(rename = "zeronOwned", alias = "mastOwned")]
    pub mast_owned: bool,
}

impl PreviewService {
    pub fn url(&self, proxy_port: u16) -> String {
        format!("http://{}:{proxy_port}", self.hostname)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshot {
    pub services: Vec<PreviewService>,
    pub proxy_port: u16,
    pub error: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub remote: bool,
}

impl Default for PreviewSnapshot {
    fn default() -> Self {
        Self {
            services: Vec::new(),
            proxy_port: PREVIEW_PROXY_PORT,
            error: None,
            project_name: None,
            remote: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPreviewsParams {
    pub chat_id: String,
}

#[cfg(test)]
mod wire_name_tests {
    use super::*;

    /// `zeronOwned` is validated by the deployed edge worker and read by older
    /// devices, so it stays the written name; `mastOwned` is also accepted.
    #[test]
    fn mast_owned_is_written_as_zeron_owned_and_reads_both() {
        let base = serde_json::json!({
            "id": "s", "projectId": "p", "projectName": "n", "projectCwd": "/c",
            "deviceId": "d", "deviceName": "D", "hostname": "h", "name": "Vite",
            "cwd": "/c", "port": 1, "pid": 2, "startedAt": 3,
        });
        for key in ["zeronOwned", "mastOwned"] {
            let mut value = base.clone();
            value[key] = true.into();
            let service: PreviewService = serde_json::from_value(value).unwrap();
            assert!(service.mast_owned);
            let written = serde_json::to_value(&service).unwrap();
            assert_eq!(written["zeronOwned"], true);
            assert!(written.get("mastOwned").is_none());
        }
    }
}
