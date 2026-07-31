//! Apple Music developer-token minting.
//!
//! Signs a short-lived MusicKit **developer token** (ES256 JWT) from a user's
//! own MusicKit credentials — the `.p8` private key, its Key ID, and the Apple
//! Team ID. This is what makes the Apple integration usable in open-source /
//! public-hosted setups: instead of pasting a hand-signed token that expires
//! every ≤6 months, the user provides the three raw Apple values once and the
//! server (which runs on their own Pin) mints and refreshes the token itself.
//!
//! Signing uses the pure-Rust `p256` crate (RustCrypto): it cross-compiles to
//! `aarch64-linux-android` with no C toolchain, accepts Apple's PKCS#8 `.p8`
//! directly, and emits the raw 64-byte `r || s` signature JWS ES256 expects.

use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;

/// Apple caps developer-token lifetime at 6 months; mint a hair under that.
pub const DEFAULT_TTL_SECS: i64 = 180 * 24 * 60 * 60;

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Mint a MusicKit developer token valid for `ttl_secs` from `now` (unix secs).
/// `p8_pem` is the `.p8` file contents (PKCS#8 PEM). Kept `now`-parameterized so
/// it is deterministically testable; callers pass the real clock at runtime.
pub fn mint_developer_token(
    p8_pem: &str,
    key_id: &str,
    team_id: &str,
    ttl_secs: i64,
    now: i64,
) -> Result<String, String> {
    if p8_pem.trim().is_empty() || key_id.trim().is_empty() || team_id.trim().is_empty() {
        return Err("apple token: p8 key, key_id, and team_id are all required".to_string());
    }

    // Apple's spec: header is exactly {alg, kid}; payload {iss, iat, exp}.
    let header = serde_json::json!({ "alg": "ES256", "kid": key_id });
    let claims = serde_json::json!({ "iss": team_id, "iat": now, "exp": now + ttl_secs });
    let signing_input = format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes()),
    );

    let key = SigningKey::from_pkcs8_pem(p8_pem)
        .map_err(|e| format!("apple token: .p8 parse failed: {e}"))?;
    // ECDSA P-256 over SHA-256 (ES256); `Signature` is the fixed 64-byte r||s.
    let sig: Signature = key.sign(signing_input.as_bytes());

    Ok(format!("{signing_input}.{}", b64url(&sig.to_bytes())))
}

/// Current unix time in seconds (runtime clock for minting).
pub fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway P-256 key in PKCS#8 PEM — structurally valid, not an Apple key.
    fn test_p8() -> String {
        use p256::pkcs8::EncodePrivateKey;
        let key = SigningKey::random(&mut rand_core::OsRng);
        key.to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[test]
    fn mint_produces_three_part_jwt_with_expected_claims() {
        let jwt =
            mint_developer_token(&test_p8(), "ABC123DEFG", "TEAM123456", 3600, 1_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[0]).unwrap(),
        )
        .unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "ABC123DEFG");

        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "TEAM123456");
        assert_eq!(claims["iat"], 1_000_000);
        assert_eq!(claims["exp"], 1_003_600);
        // Raw ES256 signature is exactly 64 bytes.
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap().len(),
            64
        );
    }

    #[test]
    fn missing_inputs_error() {
        assert!(mint_developer_token("", "k", "t", 3600, 0).is_err());
        assert!(mint_developer_token("pem", "", "t", 3600, 0).is_err());
    }

    // Mint from a REAL .p8 and prove Apple accepts the token by calling the
    // catalog API with it. Run explicitly:
    //   APPLE_P8_PATH=~/Downloads/AuthKey_XXXX.p8 APPLE_KEY_ID=XXXX \
    //   APPLE_TEAM_ID=YYYY cargo test apple_token spike_mint -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn spike_mint_real_token_and_call_apple() {
        let (Ok(p8_path), Ok(key_id), Ok(team_id)) = (
            std::env::var("APPLE_P8_PATH"),
            std::env::var("APPLE_KEY_ID"),
            std::env::var("APPLE_TEAM_ID"),
        ) else {
            eprintln!("APPLE_P8_PATH / APPLE_KEY_ID / APPLE_TEAM_ID unset; skipping");
            return;
        };
        let p8 = std::fs::read_to_string(shellexpand(&p8_path)).expect("read .p8");
        let token = mint_developer_token(&p8, &key_id, &team_id, DEFAULT_TTL_SECS, now_unix())
            .expect("mint");
        eprintln!("minted token length: {}", token.len());

        let resp = reqwest::Client::new()
            .get("https://api.music.apple.com/v1/catalog/us/songs/1440650711")
            .bearer_auth(&token)
            .send()
            .await
            .expect("apple request");
        eprintln!("apple catalog status: {}", resp.status());
        assert!(
            resp.status().is_success(),
            "Apple rejected the minted token: {}",
            resp.text().await.unwrap_or_default()
        );
    }

    fn shellexpand(p: &str) -> String {
        if let Some(rest) = p.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return format!("{home}/{rest}");
            }
        }
        p.to_string()
    }
}
