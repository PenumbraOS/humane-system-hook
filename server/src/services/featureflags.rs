use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto::featureflags::feature_flags_service_server::FeatureFlagsService;
use crate::proto::featureflags::*;

pub struct FeatureFlagsServiceImpl;

#[tonic::async_trait]
impl FeatureFlagsService for FeatureFlagsServiceImpl {
    async fn get_flags(
        &self,
        _request: Request<DeviceFeatureFlagRequest>,
    ) -> Result<Response<DeviceFeatureFlagResponse>, Status> {
        info!(">>> FeatureFlags.GetFlags");

        let flags = vec![FeatureFlagAssignment {
            flag_id: String::new(),
            flag_name: "vision_custom_gesture_enabled".into(),
            val: Some(feature_flag_assignment::Val::ValBool(true)),
        }];
        info!("  returning {} flag(s)", flags.len());

        Ok(Response::new(DeviceFeatureFlagResponse {
            assignment: flags,
        }))
    }
}
