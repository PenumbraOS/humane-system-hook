use prost::Message as _;
use tonic::{Request, Response, Status};
use tracing::info;

use super::envelope::unwrap_plaintext_data;
use crate::nearby::NearbyClient;
use crate::proto::aibus::*;
use crate::proto::common::encryption::EncryptedData;

pub struct NearbySearchHandler {
    nearby_client: NearbyClient,
}

impl NearbySearchHandler {
    pub fn new(nearby_client: NearbyClient) -> Self {
        Self { nearby_client }
    }

    pub async fn encrypted_nearby_search(
        &self,
        request: Request<EncryptedNearbySearchRequest>,
    ) -> Result<Response<EncryptedNearbySearchResponse>, Status> {
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let nearby_req = NearbySearchRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad NearbySearchRequest: {e}")))?;

        let location = nearby_req
            .location
            .ok_or_else(|| Status::invalid_argument("NearbySearchRequest missing location"))?;
        let lat = location.latitude;
        let lon = location.longitude;
        let radius = if nearby_req.radius_accuracy > 0.0 {
            nearby_req.radius_accuracy
        } else {
            1000.0
        };

        info!(
            lat = lat,
            lon = lon,
            radius = radius,
            query = %nearby_req.text_query,
            ">>> EncryptedNearbySearch"
        );

        let nearby_places = self
            .nearby_client
            .search(lat, lon, radius, &nearby_req.text_query)
            .await?;

        let result_count = nearby_places.len();
        let nearby_response = NearbySearchResponse {
            nearby_places,
            status: Some(NearbySearchResultStatus::Success as i32),
        };

        info!(results = result_count, "<<< EncryptedNearbySearch");
        Ok(Response::new(EncryptedNearbySearchResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.NearbySearchResponse",
                nearby_response.encode_to_vec(),
            )),
        }))
    }
}
