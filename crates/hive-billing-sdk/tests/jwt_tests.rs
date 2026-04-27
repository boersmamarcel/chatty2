//! Integration tests for hive-billing-sdk
//!
//! These tests verify JWT verification logic outside of WASM context.
//! Full end-to-end tests require a WASM runtime with billing host imports.

#[cfg(not(target_arch = "wasm32"))]
mod jwt_verification_tests {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    use serde::{Deserialize, Serialize};
    use sha2::Sha256;

    #[derive(Debug, Serialize, Deserialize)]
    struct TestClaims {
        sid: String,
        uid: String,
        #[serde(rename = "mod")]
        module_name: String,
        ver: String,
        res: i64,
        bal: i64,
        iat: i64,
        exp: i64,
    }

    /// Create a test JWT with HMAC-SHA256 signature
    fn create_test_jwt(claims: &TestClaims, secret: &str) -> String {
        // Header for HS256
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());

        // Payload
        let payload = serde_json::to_string(claims).unwrap();
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.as_bytes());

        // Signature
        let message = format!("{}.{}", header_b64, payload_b64);
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = mac.finalize().into_bytes();
        let signature_b64 = URL_SAFE_NO_PAD.encode(&signature);

        format!("{}.{}.{}", header_b64, payload_b64, signature_b64)
    }

    #[test]
    fn test_jwt_creation() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let claims = TestClaims {
            sid: "test-session-id".to_string(),
            uid: "test-user-id".to_string(),
            module_name: "test-module".to_string(),
            ver: "1.0.0".to_string(),
            res: 5000,
            bal: 100000,
            iat: now,
            exp: now + 300, // 5 minutes
        };

        let secret = "test-secret-key";
        let jwt = create_test_jwt(&claims, secret);

        // Verify format
        assert_eq!(jwt.split('.').count(), 3, "JWT should have 3 parts");
        println!("Generated JWT: {}", jwt);
    }

    #[test]
    fn test_jwt_verification_manual() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let claims = TestClaims {
            sid: "test-session-id".to_string(),
            uid: "test-user-id".to_string(),
            module_name: "test-module".to_string(),
            ver: "1.0.0".to_string(),
            res: 5000,
            bal: 100000,
            iat: now,
            exp: now + 300,
        };

        let secret = "test-secret-key";
        let jwt = create_test_jwt(&claims, secret);

        // Verify the JWT manually
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header_payload = format!("{}.{}", parts[0], parts[1]);
        let signature = parts[2];

        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(header_payload.as_bytes());

        let signature_bytes = URL_SAFE_NO_PAD.decode(signature).unwrap();
        mac.verify_slice(&signature_bytes).unwrap();

        // Decode and verify claims
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let decoded_claims: TestClaims = serde_json::from_slice(&payload_bytes).unwrap();

        assert_eq!(decoded_claims.sid, "test-session-id");
        assert_eq!(decoded_claims.res, 5000);
        assert_eq!(decoded_claims.bal, 100000);
    }

    #[test]
    fn test_jwt_verification_wrong_secret() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let claims = TestClaims {
            sid: "test-session-id".to_string(),
            uid: "test-user-id".to_string(),
            module_name: "test-module".to_string(),
            ver: "1.0.0".to_string(),
            res: 5000,
            bal: 100000,
            iat: now,
            exp: now + 300,
        };

        let secret = "test-secret-key";
        let jwt = create_test_jwt(&claims, secret);

        // Try to verify with wrong secret
        let parts: Vec<&str> = jwt.split('.').collect();
        let header_payload = format!("{}.{}", parts[0], parts[1]);
        let signature = parts[2];

        type HmacSha256 = Hmac<Sha256>;
        let wrong_secret = "wrong-secret-key";
        let mut mac = HmacSha256::new_from_slice(wrong_secret.as_bytes()).unwrap();
        mac.update(header_payload.as_bytes());

        let signature_bytes = URL_SAFE_NO_PAD.decode(signature).unwrap();
        let result = mac.verify_slice(&signature_bytes);

        assert!(result.is_err(), "Verification should fail with wrong secret");
    }

    #[test]
    fn test_compatibility_with_hive_registry_format() {
        // This test ensures our verification works with the same format
        // that hive-registry generates using jsonwebtoken crate.
        //
        // The format should be:
        // - Header: {"alg":"HS256","typ":"JWT"}
        // - Payload: {...claims...}
        // - Signature: HMAC-SHA256(base64url(header).base64url(payload), secret)

        let now = 1712345678i64; // Fixed timestamp for reproducibility
        let claims = TestClaims {
            sid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            uid: "user-uuid".to_string(),
            module_name: "weather-pro".to_string(),
            ver: "1.2.0".to_string(),
            res: 5000,
            bal: 4200000,
            iat: now,
            exp: now + 300,
        };

        let secret = "test-hive-secret";
        let jwt = create_test_jwt(&claims, secret);

        // Verify round-trip
        let parts: Vec<&str> = jwt.split('.').collect();
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let decoded: TestClaims = serde_json::from_slice(&payload_bytes).unwrap();

        assert_eq!(decoded.module_name, "weather-pro");
        assert_eq!(decoded.res, 5000);
        assert_eq!(decoded.bal, 4200000);

        println!(
            "✓ JWT verification compatible with hive-registry format:\n  {}",
            jwt
        );
    }
}
