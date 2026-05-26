use prost::Message as _;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto::{aibus::*, common::encryption::EncryptedData};

#[derive(Default)]
pub struct GeoLocateHandler;

impl GeoLocateHandler {
    pub async fn encrypted_geo_locate(
        &self,
        _request: Request<EncryptedGeoLocateRequest>,
    ) -> Result<Response<EncryptedGeoLocateResponse>, Status> {
        info!(">>> EncryptedGeoLocate (stub)");

        let geo_response = GeoLocateResponse {
            location: None,
            radius_accuracy: 0.0,
            status: GeoLocateResponseStatus::GeolocateResponseStatusNotFound as i32,
        };

        Ok(Response::new(EncryptedGeoLocateResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.GeoLocateResponse",
                geo_response.encode_to_vec(),
            )),
        }))
    }
}
