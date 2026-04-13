//! Stub for PartnerTokenRPCService.
//!
//! The only partner service the device uses is Tidal.
//! TidalUserManager requests access tokens and user IDs through this RPC.
//! We return a dummy long-lived token so the call follows the success path
//! and avoids the noisy UNIMPLEMENTED/404 error logs. Tidal playback won't
//! work, but nothing crashes.

use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto::partnerservices::partner_token_rpc_service_server::PartnerTokenRpcService;
use crate::proto::partnerservices::*;

pub struct PartnerServicesImpl;

#[tonic::async_trait]
impl PartnerTokenRpcService for PartnerServicesImpl {
    async fn get_token(
        &self,
        request: Request<DeviceUserAccessTokenRequest>,
    ) -> Result<Response<DeviceUserAccessTokenResponse>, Status> {
        let provider = request.into_inner().provider_name;
        info!(">>> PartnerTokenRPCService.GetToken (provider={provider})");

        // Return a long_lived_encrypted_access_token with empty values.
        // The client will try to Base64-decode + decrypt the empty access_token,
        // catch the exception, and fall back to str = "". This follows the
        // success (onNext) path, avoiding the onError path that produces the
        // noisy "Error calling partner services" / "Did not receive a token
        // response object back" error logs
        Ok(Response::new(DeviceUserAccessTokenResponse {
            access_token: Some(
                device_user_access_token_response::AccessToken::LongLivedEncryptedAccessToken(
                    AccessToken {
                        account_id: String::new(),
                        access_token: String::new(),
                        unique_provider_account_id: String::new(),
                    },
                ),
            ),
        }))
    }

    async fn get_tokens(
        &self,
        request: Request<DeviceUserAccessTokenRequest>,
    ) -> Result<Response<MultipleDeviceUserAccessTokenResponse>, Status> {
        let provider = request.into_inner().provider_name;
        info!(">>> PartnerTokenRPCService.GetTokens (provider={provider})");

        Ok(Response::new(MultipleDeviceUserAccessTokenResponse {
            tokens: vec![],
        }))
    }
}
