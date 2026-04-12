use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::Rng;
use rcgen::{CertificateParams, DistinguishedName, DnType, DnValue, Issuer, KeyPair};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use crate::proto::provisioning::device_onboarding_dac_service_server::DeviceOnboardingDacService;
use crate::proto::provisioning::*;

/// Fixed 32-byte session key — must match the OPAQUE_SESSION_KEY in IronmanHooks.kt.
/// Both the client (hooked) and server use this for AES-256-GCM in CreateDeviceUserBinding.
const SESSION_KEY: [u8; 32] = [0x42; 32];

/// Self-signed CA for signing Device User Certificates during onboarding.
///
/// Rather than parsing the device's CSR (which requires the `x509-parser` feature
/// and complex DER handling), we generate a fresh cert with the correct DN and
/// sign it with our CA. The device imports whatever cert we return without
/// validating the public key matches — `HumaneCertificate.setCertificateChain()`
/// just calls `KeyStore.setKeyEntry()` which associates the cert with the
/// existing private key regardless of whether they match.
pub struct OnboardingCa {
    pub ca_cert_der: Vec<u8>,
    pub ca_key_pair: KeyPair,
    ca_params: CertificateParams,
}

impl OnboardingCa {
    pub fn generate() -> Result<Self, Box<dyn std::error::Error>> {
        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

        let params = Self::ca_params();
        let ca_cert = params.self_signed(&key_pair)?;
        let ca_cert_der = ca_cert.der().to_vec();

        Ok(Self {
            ca_cert_der,
            ca_key_pair: key_pair,
            ca_params: params,
        })
    }

    fn ca_params() -> CertificateParams {
        let mut params = CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let mut dn = DistinguishedName::new();
        dn.push(
            DnType::CommonName,
            DnValue::Utf8String("PenumbraOS Onboarding CA".into()),
        );
        dn.push(
            DnType::OrganizationName,
            DnValue::Utf8String("PenumbraOS".into()),
        );
        params.distinguished_name = dn;
        params
    }

    /// Generate a DUC (Device User Certificate) as DER bytes.
    /// We generate a fresh keypair and cert rather than signing the device's CSR,
    /// because the device doesn't validate the public key — it just stores whatever
    /// cert chain we return alongside its existing private key.
    pub fn generate_duc(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        // Build cert params with Humane's DUC DN format
        let mut cert_params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(
            DnType::CommonName,
            DnValue::Utf8String(format!("V:01:D:{}:U:{}", device_id, user_id)),
        );
        dn.push(
            DnType::OrganizationName,
            DnValue::Utf8String("Humane".into()),
        );
        dn.push(
            DnType::OrganizationalUnitName,
            DnValue::Utf8String("DeviceUser".into()),
        );
        cert_params.distinguished_name = dn;
        cert_params.is_ca = rcgen::IsCa::NoCa;

        // Generate a keypair for the cert (the device ignores this —
        // it associates its own private key with whatever cert we return)
        let duc_key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

        // Build an Issuer from our stored CA params + key pair reference
        let issuer = Issuer::from_params(&self.ca_params, &self.ca_key_pair);

        // Sign the DUC cert with our CA
        let signed = cert_params.signed_by(&duc_key_pair, &issuer)?;

        Ok(signed.der().to_vec())
    }
}

/// AES-256-GCM encrypt using the fixed session key.
fn encrypt_aes_gcm(plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Status> {
    let cipher = Aes256Gcm::new_from_slice(&SESSION_KEY)
        .map_err(|e| Status::internal(format!("AES key error: {}", e)))?;

    let mut iv_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut iv_bytes);
    let nonce = Nonce::from_slice(&iv_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| Status::internal(format!("AES encrypt error: {}", e)))?;

    Ok((ciphertext, iv_bytes.to_vec()))
}

pub struct ProvisioningServiceImpl {
    pub ca: Arc<OnboardingCa>,
    pub display_name: String,
    pub user_id: String,
}

#[tonic::async_trait]
impl DeviceOnboardingDacService for ProvisioningServiceImpl {
    async fn get_subscription_status(
        &self,
        _request: Request<GetSubscriptionStatusRequest>,
    ) -> Result<Response<GetSubscriptionStatusResponse>, Status> {
        info!(">>> Provisioning.GetSubscriptionStatus");
        info!("  Returning ACTIVE");

        Ok(Response::new(GetSubscriptionStatusResponse {
            status: Some(SubscriptionStatus {
                status_code: SubscriptionStatusCode::Active as i32,
                message: "Active".into(),
            }),
        }))
    }

    async fn get_assigned_user_dac(
        &self,
        _request: Request<GetAssignedUserDacRequest>,
    ) -> Result<Response<GetAssignedUserResponse>, Status> {
        info!(">>> Provisioning.GetAssignedUserDAC");
        info!(
            "  Returning user_id={}, display_name={}",
            self.user_id, self.display_name
        );

        Ok(Response::new(GetAssignedUserResponse {
            user_id: self.user_id.clone(),
            display_name: self.display_name.clone(),
        }))
    }

    async fn verify_hmc_association(
        &self,
        _request: Request<VerifyHmcAssociationRequest>,
    ) -> Result<Response<VerifyHmcAssociationResponse>, Status> {
        info!(">>> Provisioning.VerifyHmcAssociation");
        info!("  Returning SUCCESS");

        Ok(Response::new(VerifyHmcAssociationResponse {
            status: HmcAssociationResponseCode::Success as i32,
        }))
    }

    async fn verify_hmc_by_pass(
        &self,
        _request: Request<VerifyHmcByPassRequest>,
    ) -> Result<Response<VerifyHmcByPassResponse>, Status> {
        info!(">>> Provisioning.VerifyHmcByPass");
        info!("  Returning ALLOWED");

        Ok(Response::new(VerifyHmcByPassResponse {
            status: VerifyHmcByPassResponseCode::Allowed as i32,
        }))
    }

    async fn create_login_init(
        &self,
        request: Request<CreateLoginInitRequest>,
    ) -> Result<Response<CreateLoginInitResponse>, Status> {
        let req = request.into_inner();
        info!(">>> Provisioning.CreateLoginInit");
        info!("  device_id={}", req.device_id);
        info!("  login_request bytes={}", req.login_request.len());

        // Return a non-empty login_response (OPAQUE KE2 placeholder).
        // The client's hooked clientLoginFinish() ignores these bytes anyway.
        let ke2_placeholder = vec![0u8; 64];

        Ok(Response::new(CreateLoginInitResponse {
            login_response: ke2_placeholder,
            response_code: Some(OpaqueLoginStatus {
                status_code: OpaqueLoginStatusCode::OpaqueLoginStatusSuccessfulRequest as i32,
                message: "OK".into(),
            }),
        }))
    }

    async fn create_login_finish(
        &self,
        request: Request<CreateLoginFinishRequest>,
    ) -> Result<Response<CreateLoginFinishResponse>, Status> {
        let req = request.into_inner();
        info!(">>> Provisioning.CreateLoginFinish");
        info!("  device_id={}", req.device_id);
        info!(
            "  login_finish_request bytes={}",
            req.login_finish_request.len()
        );
        info!(
            "  Returning user_id={}, display_name={}",
            self.user_id, self.display_name
        );

        Ok(Response::new(CreateLoginFinishResponse {
            user_id: self.user_id.clone(),
            response_code: Some(OpaqueLoginStatus {
                status_code: OpaqueLoginStatusCode::OpaqueLoginStatusSuccessfulRequest as i32,
                message: "OK".into(),
            }),
            display_name: self.display_name.clone(),
        }))
    }

    async fn create_device_user_binding(
        &self,
        request: Request<EncryptedCreateDeviceUserBindingRequest>,
    ) -> Result<Response<EncryptedCreateDeviceUserBindingResponse>, Status> {
        let req = request.into_inner();
        info!(">>> Provisioning.CreateDeviceUserBinding");

        // Log the CSR info (we don't actually use it — we generate our own cert)
        if let Some(ref csr) = req.device_user_credential_csr {
            info!(
                "  CSR: {} bytes, format={}, encoding={}",
                csr.csr.len(),
                csr.format,
                csr.encoding
            );
        } else {
            warn!("  No CSR in request (unexpected but non-fatal)");
        }

        // Generate and sign a DUC certificate
        let duc_cert_der = self
            .ca
            .generate_duc(&self.user_id, "0000dead")
            .map_err(|e| {
                warn!("  Failed to generate DUC cert: {}", e);
                Status::internal(format!("DUC cert generation failed: {}", e))
            })?;
        info!("  Generated DUC certificate: {} bytes", duc_cert_der.len());

        // Encrypt the DUC cert with the fixed session key (AES-256-GCM)
        let (encrypted_cert, iv) = encrypt_aes_gcm(&duc_cert_der)?;
        info!(
            "  Encrypted DUC cert: {} bytes, IV: {} bytes",
            encrypted_cert.len(),
            iv.len()
        );

        // Build the CA chain (just our self-signed CA cert, unencrypted)
        let ca_chain = Chain {
            cert: vec![Certificate {
                format: CertificateFormat::X509 as i32,
                encoding: CertificateEncoding::Der as i32,
                certificate: self.ca.ca_cert_der.clone(),
            }],
        };

        Ok(Response::new(EncryptedCreateDeviceUserBindingResponse {
            device_user_certificate: Some(EncryptedPayload {
                payload: encrypted_cert,
                iv,
            }),
            ca_chain: Some(ca_chain),
        }))
    }
}
