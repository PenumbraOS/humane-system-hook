use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

const USER_AGENT: &str = concat!(
    "PenumbraOS/",
    env!("PENUMBRA_VERSION"),
    " (+https://github.com/PenumbraOS/humane-system-hook)"
);

pub struct BeaconDbClient {
    http: reqwest::Client,
}

impl BeaconDbClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn locate(&self, request: &Value) -> Result<BeaconResponse, reqwest::Error> {
        self.http
            .post("https://api.beacondb.net/v1/geolocate")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .timeout(Duration::from_secs(3))
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}

#[derive(Deserialize)]
pub struct BeaconResponse {
    pub location: BeaconLocation,
    pub accuracy: f64,
}

#[derive(Deserialize)]
pub struct BeaconLocation {
    pub lat: f64,
    pub lng: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_response_json() {
        for body in [
            "not JSON",
            "{}",
            r#"{"location":{"lat":0,"lng":0},"accuracy":1e999}"#,
        ] {
            assert!(
                serde_json::from_str::<BeaconResponse>(body).is_err(),
                "{body}"
            );
        }
    }
}
