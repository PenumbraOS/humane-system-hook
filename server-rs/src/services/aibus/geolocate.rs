use std::time::Instant;

use prost::Message as _;
use reqwest::StatusCode;
use serde_json::{json, Value};
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::external::beacondb::{BeaconDbClient, BeaconResponse};
use crate::proto::{aibus::*, common::encryption::EncryptedData};

pub struct GeoLocateHandler {
    beacondb: BeaconDbClient,
}

impl GeoLocateHandler {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            beacondb: BeaconDbClient::new(http),
        }
    }

    pub async fn encrypted_geo_locate(
        &self,
        request: Request<EncryptedGeoLocateRequest>,
    ) -> Result<Response<EncryptedGeoLocateResponse>, Status> {
        let request = request.into_inner();
        let bytes = unwrap_plaintext_data(&request.request)?;
        let request = GeoLocateRequest::decode(bytes)
            .map_err(|e| Status::invalid_argument(format!("bad GeoLocateRequest: {e}")))?;
        let started = Instant::now();
        info!(
            wifi_count = request.wifi_access_points.len(),
            cell_count = request.cell_towers.len(),
            ">>> EncryptedGeoLocate (beaconDB)"
        );

        let response = geolocate_response(self.beacondb.locate(&beacon_request(&request)).await);
        info!(
            status = response.status,
            elapsed_ms = started.elapsed().as_millis(),
            "<<< EncryptedGeoLocate"
        );

        Ok(Response::new(EncryptedGeoLocateResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.GeoLocateResponse",
                response.encode_to_vec(),
            )),
        }))
    }
}

fn beacon_request(request: &GeoLocateRequest) -> Value {
    let wifi: Vec<_> = request
        .wifi_access_points
        .iter()
        .map(|ap| {
            json!({
                "macAddress": ap.mac_address,
                // beaconDB requires an integer, but Humane leaves absent signals at zero.
                "signalStrength": (-128.0..=-1.0).contains(&ap.signal_strength).then(|| ap.signal_strength.round() as i8),
            })
        })
        .collect();
    let cells: Vec<_> = request
        .cell_towers
        .iter()
        .filter(|_| matches!(request.radio_type.as_str(), "gsm" | "wcdma" | "lte"))
        .map(|cell| {
            json!({
                "cellId": cell.cell_id,
                "radioType": request.radio_type,
                "locationAreaCode": cell.location_area_code,
                "mobileCountryCode": cell.mobile_country_code,
                "mobileNetworkCode": cell.mobile_network_code,
            })
        })
        .collect();
    json!({
        "considerIp": true,
        "cellTowers": cells,
        "wifiAccessPoints": wifi,
    })
}

fn geolocate_response(result: Result<BeaconResponse, reqwest::Error>) -> GeoLocateResponse {
    use GeoLocateResponseStatus::*;

    match result {
        Ok(fix)
            if (-90.0..=90.0).contains(&fix.location.lat)
                && (-180.0..=180.0).contains(&fix.location.lng)
                && fix.accuracy.is_finite()
                && fix.accuracy > 0.0 =>
        {
            GeoLocateResponse {
                location: Some(Location {
                    latitude: fix.location.lat,
                    longitude: fix.location.lng,
                }),
                radius_accuracy: fix.accuracy,
                status: GeolocateResponseStatusSuccess as i32,
            }
        }
        Ok(_) => {
            warn!("beaconDB returned an invalid location or accuracy");
            GeoLocateResponse {
                status: GeolocateResponseStatusInternalError as i32,
                ..Default::default()
            }
        }
        Err(error) => {
            let error = error.without_url();
            warn!(%error, "beaconDB request failed");
            GeoLocateResponse {
                status: match error.status() {
                    Some(StatusCode::NOT_FOUND) => GeolocateResponseStatusNotFound,
                    Some(StatusCode::BAD_REQUEST) => GeolocateResponseStatusBadRequest,
                    _ => GeolocateResponseStatusInternalError,
                } as i32,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_observations_to_beacondb_json() {
        let request = GeoLocateRequest {
            radio_type: "lte".into(),
            wifi_access_points: vec![WifiAccessPoint {
                mac_address: "3c:37:86:5d:75:d4".into(),
                signal_strength: -51.0,
                ..Default::default()
            }],
            cell_towers: vec![CellTower {
                mobile_country_code: 310,
                mobile_network_code: 0,
                location_area_code: 42,
                cell_id: 123456,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            beacon_request(&request),
            json!({
                "considerIp": true,
                "wifiAccessPoints": [{"macAddress": "3c:37:86:5d:75:d4", "signalStrength": -51}],
                "cellTowers": [{
                    "radioType": "lte", "mobileCountryCode": 310,
                    "mobileNetworkCode": 0, "locationAreaCode": 42, "cellId": 123456
                }]
            })
        );
    }

    #[test]
    fn converts_supported_radios_without_discarding_wifi() {
        let mut request = GeoLocateRequest {
            wifi_access_points: vec![WifiAccessPoint {
                mac_address: "3c:37:86:5d:75:d4".into(),
                ..Default::default()
            }],
            cell_towers: vec![CellTower::default()],
            ..Default::default()
        };
        for (radio, count) in [
            ("gsm", 1),
            ("wcdma", 1),
            ("lte", 1),
            ("cdma", 0),
            ("nr", 0),
            ("", 0),
        ] {
            request.radio_type = radio.into();
            let body = beacon_request(&request);
            assert_eq!(
                body["cellTowers"].as_array().unwrap().len(),
                count,
                "{radio}"
            );
            assert_eq!(body["wifiAccessPoints"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn converts_signal_strengths_to_optional_integers() {
        let mut request = GeoLocateRequest {
            wifi_access_points: vec![WifiAccessPoint::default()],
            ..Default::default()
        };
        for (signal, expected) in [
            (-128.0, json!(-128)),
            (-51.6, json!(-52)),
            (-1.0, json!(-1)),
            (f64::NAN, Value::Null),
            (f64::INFINITY, Value::Null),
            (-129.0, Value::Null),
            (-0.1, Value::Null),
            (0.0, Value::Null),
            (1.0, Value::Null),
        ] {
            request.wifi_access_points[0].signal_strength = signal;
            assert_eq!(
                beacon_request(&request)["wifiAccessPoints"][0]["signalStrength"],
                expected
            );
        }
    }

    #[test]
    fn converts_a_beacondb_response_to_a_humane_location() {
        let fix = serde_json::from_str(r#"{"location":{"lat":37.7,"lng":-122.4},"accuracy":50}"#)
            .unwrap();
        let response = geolocate_response(Ok(fix));
        assert_eq!(response.status, 1);
        assert_eq!(response.radius_accuracy, 50.0);
        let location = response.location.unwrap();
        assert_eq!((location.latitude, location.longitude), (37.7, -122.4));
    }

    #[test]
    fn rejects_invalid_locations_and_accuracy() {
        for body in [
            r#"{"location":{"lat":0,"lng":0},"accuracy":0}"#,
            r#"{"location":{"lat":0,"lng":0},"accuracy":-1}"#,
            r#"{"location":{"lat":91,"lng":0},"accuracy":50}"#,
            r#"{"location":{"lat":0,"lng":181},"accuracy":50}"#,
        ] {
            let response = geolocate_response(Ok(serde_json::from_str(body).unwrap()));
            assert_eq!(response.status, 3, "{body}");
            assert!(response.location.is_none());
        }
    }

    #[test]
    fn distinguishes_location_misses_from_service_failures() {
        for (http_status, expected_status) in [
            (400, 2),
            (404, 4),
            (401, 3),
            (403, 3),
            (429, 3),
            (500, 3),
            (503, 3),
        ] {
            let http_response = http::Response::builder()
                .status(http_status)
                .body("")
                .unwrap();
            let error = reqwest::Response::from(http_response)
                .error_for_status()
                .unwrap_err();
            let response = geolocate_response(Err(error));
            assert_eq!(response.status, expected_status, "HTTP {http_status}");
            assert!(response.location.is_none());
        }
    }

    #[tokio::test]
    async fn rejects_missing_envelopes_and_invalid_protobuf() {
        let handler = GeoLocateHandler::new(reqwest::Client::new());
        for request in [
            EncryptedGeoLocateRequest::default(),
            EncryptedGeoLocateRequest {
                request: Some(EncryptedData::new(
                    "humane.aibus.GeoLocateRequest",
                    vec![0xff],
                )),
            },
        ] {
            let error = handler
                .encrypted_geo_locate(Request::new(request))
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
        }
    }
}
