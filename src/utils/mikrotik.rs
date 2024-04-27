use std::time::Duration;

use crate::config::Microtik;

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Lease {
    pub mac_address: String,
    #[serde(deserialize_with = "crate::utils::deserealize_duration")]
    pub last_seen: Duration,
}

pub async fn get_leases(
    reqwest_client: &reqwest::Client,
    conf: &Microtik,
) -> Result<Vec<Lease>, reqwest::Error> {
    let leases = reqwest_client
        .post(format!("https://{}/rest/ip/dhcp-server/lease/print", conf.host))
        .timeout(Duration::from_secs(5))
        .basic_auth(&conf.username, Some(&conf.password))
        .json(&serde_json::json!({
            ".proplist": [
                "mac-address",
                "last-seen",
            ]
        }))
        .send()
        .await?
        .json::<Vec<Lease>>()
        .await;
    crate::metrics::update_service("mikrotik", leases.is_ok());
    leases
}
