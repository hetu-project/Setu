//! User RPC Handler Implementation
//!
//! This module implements the UserRpcHandler trait for the Validator,
//! providing user-facing RPC services for wallets and DApps.
//!
//! Registration delegates to InfraExecutor for G11-compliant state changes.
//! Balance/account queries read from MerkleStateProvider (StateProvider trait).

use crate::ValidatorNetworkService;
use setu_rpc::{
    UserRpcHandler, RegisterUserRequest, RegisterUserResponse,
    GetAccountRequest, GetAccountResponse, GetBalanceRequest, GetBalanceResponse,
    GetPowerRequest, GetPowerResponse, GetFluxRequest, GetFluxResponse,
    GetCredentialsRequest, GetCredentialsResponse, TransferRequest, TransferResponse,
    CoinBalance, SubmitTransferRequest,
    // Phase 3
    UpdateProfileRequest, UpdateProfileResponse,
    GetProfileResponse, ProfileInfo,
    JoinSubnetRequest, JoinSubnetResponse,
    LeaveSubnetRequest, LeaveSubnetResponse,
    CheckMembershipResponse, GetUserSubnetsResponse,
};
use setu_types::registration::UserRegistration;
use setu_types::{ObjectId, hash_utils::setu_hash_with_domain};
use setu_types::{
    SETU_DECIMALS, SETU_SYMBOL, format_setu_units, is_setu_token_identifier,
    parse_setu_amount_to_units,
};
use setu_types::{FluxState, PowerState, flux_state_object_id, power_state_object_id, INITIAL_POWER, INITIAL_FLUX};
use setu_vlc::VLCSnapshot;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn, error};

/// User RPC Handler for Validator
pub struct ValidatorUserHandler {
    /// Reference to the network service
    network_service: Arc<ValidatorNetworkService>,
}

impl ValidatorUserHandler {
    /// Create a new user handler
    pub fn new(network_service: Arc<ValidatorNetworkService>) -> Self {
        Self { network_service }
    }

    /// Build error response for register_user
    fn reg_err(message: &str, address: &str) -> RegisterUserResponse {
        RegisterUserResponse {
            success: false,
            message: message.to_string(),
            address: address.to_string(),
            event_id: None,
            initial_setu: 0,
            initial_power: 0,
            initial_flux: 0,
        }
    }

    /// Verify signature for a write operation (3-branch: MetaMask / Setu native / Nostr).
    /// Returns Ok(()) if valid, or error message string if invalid.
    fn verify_signature(
        address: &str,
        signature: &[u8],
        message: &str,
        nostr_pubkey: Option<&[u8]>,
        public_key: Option<&str>,
    ) -> Result<(), String> {
        if std::env::var("SETU_SKIP_SIG_VERIFY").unwrap_or_default() == "1" {
            return Ok(());
        }
        let result = if let Some(npk) = nostr_pubkey {
            setu_keys::verify::verify_nostr_schnorr(address, npk, signature, message.as_bytes())
        } else if let Some(pk_b64) = public_key {
            let pk_raw = setu_keys::PublicKey::decode_base64(pk_b64)
                .and_then(|pk| {
                    let mut v = vec![pk.scheme().flag()];
                    v.extend(pk.as_bytes());
                    Ok(v)
                });
            match pk_raw {
                Ok(pk_bytes) => setu_keys::verify::verify_setu_native_raw(
                    address, &pk_bytes, signature, message.as_bytes(),
                ),
                Err(e) => Err(e),
            }
        } else {
            setu_keys::verify::verify_metamask_personal_sign(address, signature, message)
        };
        result.map_err(|e| format!("Signature verification failed: {}", e))
    }

    /// Build VLC snapshot for a new event
    fn build_vlc_snapshot(&self) -> VLCSnapshot {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let vlc_time = self.network_service.get_vlc_time();
        let mut vlc = setu_vlc::VectorClock::new();
        vlc.increment(self.network_service.validator_id());
        VLCSnapshot {
            vector_clock: vlc,
            logical_time: vlc_time,
            physical_time: now,
        }
    }

    /// Validate timestamp is within 5-minute anti-replay window
    fn check_timestamp(timestamp: u64) -> Result<(), String> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let req_secs = timestamp / 1000;
        if now_secs.abs_diff(req_secs) > 300 {
            return Err("Timestamp too old or too far in the future (5 min window)".to_string());
        }
        Ok(())
    }

    fn transfer_err(message: &str) -> TransferResponse {
        TransferResponse {
            success: false,
            message: message.to_string(),
            event_id: None,
            estimated_confirmation: None,
        }
    }

    fn is_user_address(address: &str) -> bool {
        address.starts_with("0x") && (address.len() == 66 || address.len() == 42)
    }

    fn resolve_transfer_amount(request: &TransferRequest, coin_type: &str) -> Result<u64, String> {
        if !is_setu_token_identifier(coin_type) {
            return Err(
                "Signed user transfers currently support SETU only; non-SETU token transfers are deferred"
                    .to_string(),
            );
        }

        let parsed_display = match request.display_amount.as_deref() {
            Some(display_amount) => {
                Some(
                    parse_setu_amount_to_units(display_amount)
                        .map_err(|e| format!("Invalid display_amount: {}", e))?,
                )
            }
            None => None,
        };

        match (request.amount, parsed_display) {
            (Some(raw), Some(display_units)) if raw != display_units => Err(
                "amount and display_amount do not match after SETU decimal conversion".to_string(),
            ),
            (Some(raw), _) => Ok(raw),
            (None, Some(display_units)) => Ok(display_units),
            (None, None) => Err("Transfer amount is required".to_string()),
        }
    }

    fn coin_type_matches_filter(coin_type: &str, filter: &str) -> bool {
        if is_setu_token_identifier(filter) {
            is_setu_token_identifier(coin_type)
        } else {
            coin_type == filter
        }
    }

    fn display_coin_balance(coin_type: String, balance: u64, coin_count: u32) -> CoinBalance {
        if is_setu_token_identifier(&coin_type) {
            CoinBalance {
                coin_type,
                balance,
                coin_count,
                symbol: SETU_SYMBOL.to_string(),
                decimals: SETU_DECIMALS,
                display_balance: format_setu_units(balance),
            }
        } else {
            CoinBalance {
                symbol: coin_type.clone(),
                coin_type,
                balance,
                coin_count,
                decimals: 0,
                display_balance: balance.to_string(),
            }
        }
    }

    /// V2 canonical signing message (design D3). Binds every authorization
    /// axis: chain, normalized addresses, raw amount, canonical subnet,
    /// client nonce, and timestamp. Variable-length fields carry length
    /// prefixes; `subnet_canonical` is "ROOT" or full 0x+64-hex, never a
    /// public id or short display form.
    fn canonical_transfer_message_v2(
        chain_id: &str,
        normalized_from: &str,
        normalized_to: &str,
        amount_raw: u64,
        subnet_canonical: &str,
        client_nonce: &str,
        timestamp_ms: u64,
    ) -> String {
        format!(
            "SETU_TRANSFER_V2\n\
             chain_id_len={};chain_id={}\n\
             from_len={};from={}\n\
             to_len={};to={}\n\
             amount_raw={}\n\
             subnet_id_len={};subnet_id={}\n\
             client_nonce_len={};client_nonce={}\n\
             timestamp_ms={}",
            chain_id.len(), chain_id,
            normalized_from.len(), normalized_from,
            normalized_to.len(), normalized_to,
            amount_raw,
            subnet_canonical.len(), subnet_canonical,
            client_nonce.len(), client_nonce,
            timestamp_ms,
        )
    }

    /// Lowercase-normalize a user address for signing and marker derivation
    fn normalize_address(address: &str) -> String {
        address.to_lowercase()
    }

    /// Resolve the raw amount for a non-ROOT subnet transfer (design D6):
    /// raw `amount` required, `display_amount` forbidden in this phase.
    fn resolve_subnet_transfer_amount(request: &TransferRequest) -> Result<u64, String> {
        if request.display_amount.is_some() {
            return Err(
                "display_amount is not supported for subnet token transfer; use raw amount units"
                    .to_string(),
            );
        }
        match request.amount {
            Some(raw) if raw > 0 => Ok(raw),
            Some(_) => Err("Transfer amount must be greater than zero".to_string()),
            None => Err("Transfer amount (raw units) is required for subnet token transfer".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ValidatorUserHandler;
    use setu_keys::{SetuKeyPair, SignatureScheme};

    const TEST_CHAIN: &str = "setu-dev";

    #[allow(clippy::too_many_arguments)]
    fn sign_v2(
        keypair: &SetuKeyPair,
        chain_id: &str,
        from: &str,
        to: &str,
        amount: u64,
        subnet_canonical: &str,
        nonce: &str,
        timestamp: u64,
    ) -> (String, Vec<u8>) {
        let message = ValidatorUserHandler::canonical_transfer_message_v2(
            chain_id,
            &from.to_lowercase(),
            &to.to_lowercase(),
            amount,
            subnet_canonical,
            nonce,
            timestamp,
        );
        let signature = keypair.sign(message.as_bytes());
        let mut signature_bytes = vec![signature.scheme().flag()];
        signature_bytes.extend(signature.as_bytes());
        (message, signature_bytes)
    }

    // Test #4 (design §9): V2 canonical message binds every authorization axis
    #[test]
    fn transfer_canonical_message_v2_binds_all_fields() {
        let message = ValidatorUserHandler::canonical_transfer_message_v2(
            "setu-dev",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            42,
            "ROOT",
            "nonce-0001",
            1778390000000,
        );

        assert_eq!(
            message,
            "SETU_TRANSFER_V2\n\
             chain_id_len=8;chain_id=setu-dev\n\
             from_len=66;from=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             to_len=66;to=0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             amount_raw=42\n\
             subnet_id_len=4;subnet_id=ROOT\n\
             client_nonce_len=10;client_nonce=nonce-0001\n\
             timestamp_ms=1778390000000"
        );
    }

    // Test #5 (design §9): tampering the subnet axis invalidates the signature
    #[test]
    fn transfer_v2_tampered_subnet_fails_signature() {
        std::env::remove_var("SETU_SKIP_SIG_VERIFY");
        let keypair = SetuKeyPair::generate(SignatureScheme::ED25519);
        let from = keypair.address().to_hex();
        let to = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let subnet = setu_types::SubnetId::from_str_id("gaming-subnet").canonical_string();
        let other_subnet = setu_types::SubnetId::from_str_id("other-subnet").canonical_string();
        let (_, signature) =
            sign_v2(&keypair, TEST_CHAIN, &from, to, 7, &subnet, "nonce-0001", 1778390000000);

        // The validator rebuilds the message with the request's subnet; if an
        // attacker swaps the subnet, the rebuilt message no longer matches
        // what was signed.
        let tampered = ValidatorUserHandler::canonical_transfer_message_v2(
            TEST_CHAIN, &from.to_lowercase(), &to.to_lowercase(), 7,
            &other_subnet, "nonce-0001", 1778390000000,
        );
        let result = ValidatorUserHandler::verify_signature(
            &from, &signature, &tampered, None, Some(&keypair.public().encode_base64()),
        );
        assert!(result.is_err(), "subnet tamper must invalidate the signature");
    }

    // Test #6 (design §9): tampering the amount axis invalidates the signature
    #[test]
    fn transfer_v2_tampered_amount_fails_signature() {
        std::env::remove_var("SETU_SKIP_SIG_VERIFY");
        let keypair = SetuKeyPair::generate(SignatureScheme::ED25519);
        let from = keypair.address().to_hex();
        let to = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let (_, signature) =
            sign_v2(&keypair, TEST_CHAIN, &from, to, 7, "ROOT", "nonce-0001", 1778390000000);

        let tampered = ValidatorUserHandler::canonical_transfer_message_v2(
            TEST_CHAIN, &from.to_lowercase(), &to.to_lowercase(), 700,
            "ROOT", "nonce-0001", 1778390000000,
        );
        let result = ValidatorUserHandler::verify_signature(
            &from, &signature, &tampered, None, Some(&keypair.public().encode_base64()),
        );
        assert!(result.is_err(), "amount tamper must invalidate the signature");
    }

    #[test]
    fn transfer_setu_native_signature_accepts_matching_address() {
        std::env::remove_var("SETU_SKIP_SIG_VERIFY");
        let keypair = SetuKeyPair::generate(SignatureScheme::ED25519);
        let from = keypair.address().to_hex();
        let to = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let (message, signature) =
            sign_v2(&keypair, TEST_CHAIN, &from, to, 7, "ROOT", "nonce-0001", 1778390000000);

        let result = ValidatorUserHandler::verify_signature(
            &from,
            &signature,
            &message,
            None,
            Some(&keypair.public().encode_base64()),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn transfer_setu_native_signature_rejects_wrong_address() {
        std::env::remove_var("SETU_SKIP_SIG_VERIFY");
        let keypair = SetuKeyPair::generate(SignatureScheme::ED25519);
        let wrong_from = "0x3333333333333333333333333333333333333333333333333333333333333333";
        let to = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let (message, signature) =
            sign_v2(&keypair, TEST_CHAIN, wrong_from, to, 7, "ROOT", "nonce-0001", 1778390000000);

        let result = ValidatorUserHandler::verify_signature(
            wrong_from,
            &signature,
            &message,
            None,
            Some(&keypair.public().encode_base64()),
        );

        assert!(result.is_err());
    }

    fn base_request() -> setu_rpc::TransferRequest {
        setu_rpc::TransferRequest {
            from: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            to: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            amount: Some(123_000_000),
            display_amount: None,
            coin_type: Some("setu".to_string()),
            subnet_id: None,
            client_nonce: Some("nonce-0001".to_string()),
            chain_id: Some(TEST_CHAIN.to_string()),
            memo: None,
            message: None,
            timestamp: 1778390000000,
            signature: None,
            public_key: None,
            nostr_pubkey: None,
        }
    }

    #[test]
    fn transfer_amount_accepts_raw_units() {
        let request = base_request();
        assert_eq!(
            ValidatorUserHandler::resolve_transfer_amount(&request, "setu"),
            Ok(123_000_000)
        );
    }

    #[test]
    fn transfer_amount_accepts_setu_display_amount() {
        let mut request = base_request();
        request.amount = None;
        request.display_amount = Some("1.23".to_string());
        assert_eq!(
            ValidatorUserHandler::resolve_transfer_amount(&request, "setu"),
            Ok(123_000_000)
        );
    }

    #[test]
    fn transfer_amount_rejects_raw_display_mismatch() {
        let mut request = base_request();
        request.amount = Some(120_000_000);
        request.display_amount = Some("1.23".to_string());
        let error = ValidatorUserHandler::resolve_transfer_amount(&request, "setu")
            .expect_err("mismatched amount forms must be rejected");
        assert!(error.contains("do not match"));
    }

    #[test]
    fn transfer_amount_rejects_non_setu_raw_amount() {
        let mut request = base_request();
        request.coin_type = Some("game".to_string());
        let error = ValidatorUserHandler::resolve_transfer_amount(&request, "game")
            .expect_err("non-SETU raw amount must be rejected in SETU-only user transfer path");
        assert!(error.contains("SETU only"));
    }

    #[test]
    fn transfer_amount_rejects_non_setu_display_amount() {
        let mut request = base_request();
        request.amount = None;
        request.display_amount = Some("1.23".to_string());
        request.coin_type = Some("game".to_string());
        let error = ValidatorUserHandler::resolve_transfer_amount(&request, "game")
            .expect_err("non-SETU display amount must be rejected in first implementation");
        assert!(error.contains("SETU only"));
    }

    // ---- Full admission-flow tests (#7, #8, #9, #20) ----

    fn create_test_handler() -> ValidatorUserHandler {
        let service = std::sync::Arc::new(crate::ValidatorNetworkService::new(
            "test-validator".to_string(),
            std::sync::Arc::new(crate::RouterManager::new()),
            std::sync::Arc::new(crate::TaskPreparer::new_for_testing("test-validator".to_string())),
            std::sync::Arc::new(crate::BatchTaskPreparer::new_for_testing(
                "test-validator".to_string(),
            )),
            crate::NetworkServiceConfig::default(),
        ));
        ValidatorUserHandler::new(service)
    }

    fn register_test_subnet(handler: &ValidatorUserHandler, public_id: &str) {
        let registration = setu_types::registration::SubnetRegistration::new(
            public_id.to_string(),
            public_id.to_string(),
            "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
            "TST".to_string(),
        );
        handler.network_service.add_subnet(
            crate::network::SubnetInfo::from_registration(&registration, 1_778_390_000_000),
        );
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    // Test #7 (design §9): missing client_nonce fails
    #[tokio::test]
    async fn transfer_v2_missing_nonce_rejected() {
        use setu_rpc::UserRpcHandler;
        let handler = create_test_handler();
        let mut request = base_request();
        request.client_nonce = None;
        request.timestamp = now_ms();

        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(response.message.contains("client_nonce is required"), "{}", response.message);
    }

    // Test #8 (design §9): display_amount on a subnet transfer fails
    #[tokio::test]
    async fn transfer_v2_subnet_display_amount_rejected() {
        use setu_rpc::UserRpcHandler;
        let handler = create_test_handler();
        register_test_subnet(&handler, "gaming-subnet");

        let mut request = base_request();
        request.subnet_id = Some("gaming-subnet".to_string());
        request.coin_type = None;
        request.amount = None;
        request.display_amount = Some("1.23".to_string());
        request.timestamp = now_ms();

        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(
            response.message.contains("display_amount is not supported"),
            "{}",
            response.message
        );
    }

    // Test #9 (design §9): coin_type combined with a non-ROOT subnet fails
    #[tokio::test]
    async fn transfer_v2_subnet_with_coin_type_rejected() {
        use setu_rpc::UserRpcHandler;
        let handler = create_test_handler();
        register_test_subnet(&handler, "gaming-subnet");

        let mut request = base_request();
        request.subnet_id = Some("gaming-subnet".to_string());
        request.coin_type = Some("game".to_string());
        request.timestamp = now_ms();

        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(
            response.message.contains("coin_type must be omitted"),
            "{}",
            response.message
        );
    }

    // Test #20 (design §9): existence check works by canonical id — the
    // full-hex form of a registered public id passes the registry gate, while
    // a phantom (typo) public id resolves but fails it.
    #[tokio::test]
    async fn transfer_v2_subnet_existence_checked_by_canonical_id() {
        use setu_rpc::UserRpcHandler;
        let handler = create_test_handler();
        register_test_subnet(&handler, "gaming-subnet");

        // Full-hex addressing of the registered subnet: passes the registry
        // gate and proceeds to the signed-message requirement (a later check),
        // proving admission did not false-reject the hex form.
        let canonical = setu_types::SubnetId::from_str_id("gaming-subnet").canonical_string();
        let mut request = base_request();
        request.subnet_id = Some(canonical);
        request.coin_type = None;
        request.timestamp = now_ms();
        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(
            response.message.contains("Signed message is required"),
            "full-hex form must pass the registry check; got: {}",
            response.message
        );

        // Phantom subnet: resolves to a well-formed SubnetId but is not
        // registered — admission fails closed at the registry gate.
        let mut request = base_request();
        request.subnet_id = Some("gaming-subnte".to_string());
        request.coin_type = None;
        request.timestamp = now_ms();
        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(
            response.message.contains("Unknown subnet"),
            "phantom subnet must fail the registry check; got: {}",
            response.message
        );
    }

    // Invalid subnet grammar is rejected at resolution (D1 rule 4)
    #[tokio::test]
    async fn transfer_v2_invalid_subnet_grammar_rejected() {
        use setu_rpc::UserRpcHandler;
        let handler = create_test_handler();
        let mut request = base_request();
        request.subnet_id = Some("Bad_Subnet!".to_string());
        request.coin_type = None;
        request.timestamp = now_ms();

        let response = handler.transfer(request).await;
        assert!(!response.success);
        assert!(response.message.contains("Invalid subnet_id"), "{}", response.message);
    }

    #[test]
    fn coin_type_filter_matches_setu_aliases() {
        assert!(ValidatorUserHandler::coin_type_matches_filter("ROOT", "setu"));
        assert!(ValidatorUserHandler::coin_type_matches_filter("setu", "ROOT"));
        assert!(!ValidatorUserHandler::coin_type_matches_filter("game", "setu"));
        assert!(ValidatorUserHandler::coin_type_matches_filter("game", "game"));
    }

    #[test]
    fn display_coin_balance_formats_setu_units() {
        let balance = ValidatorUserHandler::display_coin_balance("ROOT".to_string(), 123_000_000, 2);

        assert_eq!(balance.symbol, "SETU");
        assert_eq!(balance.decimals, 8);
        assert_eq!(balance.display_balance, "1.23");
    }

    #[test]
    fn display_coin_balance_falls_back_for_non_setu() {
        let balance = ValidatorUserHandler::display_coin_balance("game".to_string(), 12345, 1);

        assert_eq!(balance.symbol, "game");
        assert_eq!(balance.decimals, 0);
        assert_eq!(balance.display_balance, "12345");
    }
}

#[async_trait::async_trait]
impl UserRpcHandler for ValidatorUserHandler {
    async fn register_user(&self, request: RegisterUserRequest) -> RegisterUserResponse {
        info!(
            address = %request.address,
            subnet_id = ?request.subnet_id,
            is_metamask = %request.nostr_pubkey.is_none(),
            "Processing user registration request"
        );

        // ── Step 1: Validate request ────────────────────────────────
        if request.address.is_empty() {
            return Self::reg_err("Wallet address cannot be empty", &request.address);
        }

        // Accept 66-char Setu native (0x + 64 hex) or 42-char Ethereum (0x + 40 hex)
        if !request.address.starts_with("0x")
            || (request.address.len() != 66 && request.address.len() != 42)
        {
            return Self::reg_err(
                "Invalid address format: expected 0x + 64 hex (Setu) or 0x + 40 hex (Ethereum)",
                &request.address,
            );
        }

        // Nostr-specific validation
        if let Some(ref nostr_pubkey) = request.nostr_pubkey {
            if nostr_pubkey.len() != 32 {
                return Self::reg_err("Nostr public key must be 32 bytes", &request.address);
            }
            if request.signature.is_none() || request.signature.as_ref().unwrap().is_empty() {
                return Self::reg_err("Nostr signature cannot be empty", &request.address);
            }
        }

        // ── Step 2: Timestamp anti-replay ──────────────────────────
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let req_secs = request.timestamp / 1000; // request.timestamp is millis
        if now_secs.abs_diff(req_secs) > 300 {
            return Self::reg_err(
                "Timestamp too old or too far in the future (5 min window)",
                &request.address,
            );
        }

        // ── Step 3: Signature verification ──────────────────────────
        let skip_sig = std::env::var("SETU_SKIP_SIG_VERIFY").unwrap_or_default() == "1";

        if !skip_sig {
            let message = match &request.message {
                Some(m) => m.clone(),
                None => {
                    return Self::reg_err(
                        "Signed message is required for registration",
                        &request.address,
                    );
                }
            };

            let signature = match &request.signature {
                Some(s) if !s.is_empty() => s,
                _ => {
                    return Self::reg_err(
                        "Signature is required for registration",
                        &request.address,
                    );
                }
            };

            let sig_result = if let Some(ref nostr_pubkey) = request.nostr_pubkey {
                // Nostr: Schnorr BIP-340
                setu_keys::verify::verify_nostr_schnorr(
                    &request.address,
                    nostr_pubkey,
                    signature,
                    message.as_bytes(),
                )
            } else if let Some(ref public_key_b64) = request.public_key {
                // Setu native: Ed25519 / Secp256k1 / Secp256r1
                // public_key is base64 (flag || pk_bytes), signature is raw bytes.
                let pk_raw = setu_keys::PublicKey::decode_base64(public_key_b64)
                    .and_then(|pk| {
                        let mut v = vec![pk.scheme().flag()];
                        v.extend(pk.as_bytes());
                        Ok(v)
                    });
                match pk_raw {
                    Ok(pk_bytes) => setu_keys::verify::verify_setu_native_raw(
                        &request.address,
                        &pk_bytes,
                        signature,
                        message.as_bytes(),
                    ),
                    Err(e) => Err(e),
                }
            } else {
                // MetaMask: secp256k1 ECDSA with personal_sign recovery
                setu_keys::verify::verify_metamask_personal_sign(
                    &request.address,
                    signature,
                    &message,
                )
            };

            if let Err(e) = sig_result {
                warn!(address = %request.address, error = %e, "Signature verification failed");
                return Self::reg_err(
                    &format!("Signature verification failed: {}", e),
                    &request.address,
                );
            }
        }

        // ── Step 4: Duplicate registration detection ────────────────
        let subnet_id = request.subnet_id.as_deref().unwrap_or("subnet-0");
        let membership_key = format!("user:{}:subnet:{}", request.address, subnet_id);
        let membership_object_id = ObjectId::new(
            setu_hash_with_domain(b"SETU_MEMBERSHIP:", membership_key.as_bytes()),
        );

        if self
            .network_service
            .state_provider()
            .get_object(&membership_object_id)
            .is_some()
        {
            return Self::reg_err(
                &format!(
                    "User {} already registered in subnet '{}'",
                    request.address, subnet_id
                ),
                &request.address,
            );
        }

        // ── Step 5: Build VLC snapshot ──────────────────────────────
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let vlc_time = self.network_service.get_vlc_time();
        let mut vlc = setu_vlc::VectorClock::new();
        vlc.increment(self.network_service.validator_id());
        let vlc_snapshot = VLCSnapshot {
            vector_clock: vlc,
            logical_time: vlc_time,
            physical_time: now,
        };

        // ── Step 6: Build UserRegistration ──────────────────────────
        let registration = UserRegistration {
            address: request.address.clone(),
            nostr_pubkey: request.nostr_pubkey.clone(),
            signature: request.signature.clone(),
            message: request.message.clone(),
            timestamp: request.timestamp,
            subnet_id: request.subnet_id.clone(),
            display_name: request.display_name.clone(),
            metadata: request.metadata.clone(),
            invited_by: None,
            invite_code: request.invite_code.clone(),
            public_key: request.public_key.clone(),
        };

        // ── Step 7: Delegate to InfraExecutor (路径 B) ──────────────
        // InfraExecutor:
        //   → RuntimeExecutor::execute_user_register()  (G11-compliant "oid:{hex}" state keys)
        //   → returns Event with execution_result set; CF finalization applies state
        let event = match self
            .network_service
            .infra_executor()
            .execute_user_register(&registration, vlc_snapshot)
        {
            Ok(event) => event,
            Err(e) => {
                error!(address = %request.address, error = %e, "InfraExecutor user registration failed");
                return Self::reg_err(&format!("Registration failed: {}", e), &request.address);
            }
        };

        let event_id = event.id.clone();

        // ── Step 8: Add event to DAG ────────────────────────────────
        let submit_response = self.network_service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                address = %request.address,
                message = %submit_response.message,
                "User registration DAG submission failed"
            );
            return Self::reg_err(&submit_response.message, &request.address);
        }

        info!(
            address = %request.address,
            event_id = %event_id,
            "User registered successfully (zero initial balance — use Faucet for tokens)"
        );

        RegisterUserResponse {
            success: true,
            message: "User registered successfully".to_string(),
            address: request.address,
            event_id: Some(event_id),
            initial_setu: 0,
            initial_power: INITIAL_POWER,
            initial_flux: INITIAL_FLUX,
        }
    }
    
    async fn get_account(&self, request: GetAccountRequest) -> GetAccountResponse {
        info!(address = %request.address, "Getting account information");

        let coins = self.network_service.state_provider().get_coins_for_address(&request.address);
        let setu_balance: u64 = coins.iter()
            .filter(|c| is_setu_token_identifier(&c.coin_type))
            .map(|c| c.balance)
            .sum();

        // Read Power from Merkle tree
        let power_oid = power_state_object_id(&request.address);
        let power = self.network_service.state_provider()
            .get_object(&power_oid)
            .and_then(|bytes| serde_json::from_slice::<PowerState>(&bytes).ok())
            .map(|ps| ps.power_remaining)
            .unwrap_or(0);

        // Read Flux from Merkle tree
        let flux_oid = flux_state_object_id(&request.address);
        let flux = self.network_service.state_provider()
            .get_object(&flux_oid)
            .and_then(|bytes| serde_json::from_slice::<FluxState>(&bytes).ok())
            .map(|fs| fs.flux)
            .unwrap_or(0);

        GetAccountResponse {
            found: !coins.is_empty(),
            address: request.address,
            setu_balance,
            setu_decimals: SETU_DECIMALS,
            setu_display_balance: format_setu_units(setu_balance),
            power,
            flux,
            profile: None,
            credential_count: 0,
        }
    }
    
    async fn get_balance(&self, request: GetBalanceRequest) -> GetBalanceResponse {
        info!(address = %request.address, "Getting balance");

        // Subnet filter (design D11): resolve canonically and filter by
        // SubnetId compare, not by coin_type string matching.
        let coins = match request.subnet_id.as_deref() {
            Some(s) => match setu_types::SubnetId::parse_public_or_hex(s) {
                Ok(subnet) => self
                    .network_service
                    .state_provider()
                    .get_coins_for_address_in_subnet(&request.address, &subnet),
                Err(_) => Vec::new(), // invalid subnet → no balances, not ROOT fallback
            },
            None => self
                .network_service
                .state_provider()
                .get_coins_for_address(&request.address),
        };

        // Aggregate by coin_type
        let mut type_map: std::collections::HashMap<String, (u64, u32)> = std::collections::HashMap::new();
        for c in &coins {
            let entry = type_map.entry(c.coin_type.clone()).or_insert((0, 0));
            entry.0 += c.balance;
            entry.1 += 1;
        }

        // Optional filter by coin_type
        let mut balances: Vec<CoinBalance> = type_map.into_iter()
            .filter(|(ct, _)| {
                request.coin_type.as_ref().map_or(true, |filter| {
                    Self::coin_type_matches_filter(ct, filter)
                })
            })
            .map(|(coin_type, (balance, coin_count))| {
                Self::display_coin_balance(coin_type, balance, coin_count)
            })
            .collect();
        balances.sort_by(|a, b| a.coin_type.cmp(&b.coin_type).then(a.symbol.cmp(&b.symbol)));

        let total_balance = balances.iter().map(|b| b.balance).sum();
        let (total_display_balance, total_decimals, total_symbol) = if balances.len() == 1 {
            let balance = &balances[0];
            (
                Some(balance.display_balance.clone()),
                Some(balance.decimals),
                Some(balance.symbol.clone()),
            )
        } else {
            (None, None, None)
        };

        GetBalanceResponse {
            found: !coins.is_empty(),
            address: request.address,
            balances,
            total_balance,
            total_display_balance,
            total_decimals,
            total_symbol,
        }
    }
    
    async fn get_power(&self, request: GetPowerRequest) -> GetPowerResponse {
        let power_oid = power_state_object_id(&request.address);
        match self.network_service.state_provider()
            .get_object(&power_oid)
            .and_then(|bytes| serde_json::from_slice::<PowerState>(&bytes).ok())
        {
            Some(ps) => GetPowerResponse {
                found: true,
                address: request.address,
                power: ps.power_remaining,
                rank: None,
                recent_changes: vec![],
            },
            None => GetPowerResponse {
                found: false,
                address: request.address,
                power: 0,
                rank: None,
                recent_changes: vec![],
            },
        }
    }
    
    async fn get_flux(&self, request: GetFluxRequest) -> GetFluxResponse {
        let flux_oid = flux_state_object_id(&request.address);
        match self.network_service.state_provider()
            .get_object(&flux_oid)
            .and_then(|bytes| serde_json::from_slice::<FluxState>(&bytes).ok())
        {
            Some(fs) => GetFluxResponse {
                found: true,
                address: request.address,
                flux: fs.flux,
                level: None,
                recent_changes: vec![],
            },
            None => GetFluxResponse {
                found: false,
                address: request.address,
                flux: 0,
                level: None,
                recent_changes: vec![],
            },
        }
    }
    
    async fn get_credentials(&self, request: GetCredentialsRequest) -> GetCredentialsResponse {
        // Credential system not yet implemented — return empty
        GetCredentialsResponse {
            found: false,
            address: request.address,
            credentials: vec![],
            valid_count: 0,
        }
    }
    
    async fn transfer(&self, request: TransferRequest) -> TransferResponse {
        // --- Address validation + normalization (D3.1) ---
        if !Self::is_user_address(&request.from) {
            return Self::transfer_err(
                "Invalid from address format: expected 0x + 64 hex (Setu) or 0x + 40 hex (Ethereum)",
            );
        }
        if !Self::is_user_address(&request.to) {
            return Self::transfer_err(
                "Invalid to address format: expected 0x + 64 hex (Setu) or 0x + 40 hex (Ethereum)",
            );
        }
        let normalized_from = Self::normalize_address(&request.from);
        let normalized_to = Self::normalize_address(&request.to);

        // --- Subnet resolution (D1): fallible, no ROOT fallback ---
        let subnet_id = match request.subnet_id.as_deref() {
            None => setu_types::SubnetId::ROOT,
            Some(s) => match setu_types::SubnetId::parse_public_or_hex(s) {
                Ok(id) => id,
                Err(e) => return Self::transfer_err(&format!("Invalid subnet_id: {}", e)),
            },
        };
        let subnet_canonical = subnet_id.canonical_string();

        // --- Amount + namespace rules (D2/D6) ---
        let amount_units = if subnet_id.is_root() {
            let coin_type = request
                .coin_type
                .clone()
                .unwrap_or_else(|| "setu".to_string())
                .to_lowercase();
            match Self::resolve_transfer_amount(&request, &coin_type) {
                Ok(amount) => amount,
                Err(e) => return Self::transfer_err(&e),
            }
        } else {
            // D2.5: token symbols are not execution namespaces
            if request.coin_type.is_some() {
                return Self::transfer_err(
                    "coin_type must be omitted for subnet token transfer; the subnet_id determines the token",
                );
            }
            // D7: fail closed on unregistered subnets, checked by canonical id
            if self.network_service.get_subnet_info_by_canonical(&subnet_id).is_none() {
                return Self::transfer_err("Unknown subnet: not registered or not active");
            }
            match Self::resolve_subnet_transfer_amount(&request) {
                Ok(amount) => amount,
                Err(e) => return Self::transfer_err(&e),
            }
        };
        if amount_units == 0 {
            return Self::transfer_err("Transfer amount must be greater than zero");
        }

        // --- V2 nonce + chain binding (D2.6/D2.7) ---
        let client_nonce = match request.client_nonce.as_deref() {
            Some(nonce) => match crate::user_transfer_nonce::validate_client_nonce(nonce) {
                Ok(()) => nonce.to_string(),
                Err(e) => return Self::transfer_err(&e),
            },
            None => return Self::transfer_err("client_nonce is required for V2 signed transfer"),
        };
        let chain_id = self.network_service.chain_id().to_string();
        match request.chain_id.as_deref() {
            Some(c) if c == chain_id => {}
            Some(_) => return Self::transfer_err("chain_id does not match this validator's chain"),
            None => return Self::transfer_err("chain_id is required for V2 signed transfer"),
        }

        // --- Freshness window (D3.5: not the replay defense) ---
        if let Err(e) = Self::check_timestamp(request.timestamp) {
            return Self::transfer_err(&e);
        }

        info!(
            from = %request.from,
            to = %request.to,
            amount = amount_units,
            subnet = %subnet_canonical,
            "Processing V2 transfer request"
        );

        // --- Canonical V2 message + signature (D3) ---
        let expected_message = Self::canonical_transfer_message_v2(
            &chain_id,
            &normalized_from,
            &normalized_to,
            amount_units,
            &subnet_canonical,
            &client_nonce,
            request.timestamp,
        );
        let message = match request.message.as_deref() {
            Some(message) if !message.is_empty() => message,
            _ => return Self::transfer_err("Signed message is required for transfer"),
        };
        if message != expected_message {
            return Self::transfer_err("Transfer signed message does not match request fields");
        }

        let signature = match request.signature.as_deref() {
            Some(signature) if !signature.is_empty() => signature,
            _ => return Self::transfer_err("Signature is required for transfer"),
        };
        if let Err(e) = Self::verify_signature(
            &request.from,
            signature,
            message,
            request.nostr_pubkey.as_deref(),
            request.public_key.as_deref(),
        ) {
            warn!(from = %request.from, error = %e, "Transfer signature verification failed");
            return Self::transfer_err(&e);
        }

        // --- Durable anti-replay precheck (D4 rule 1, overlay-merged view).
        // Best-effort early rejection; the authoritative gate is the apply
        // conflict check on the marker create.
        let marker_id =
            crate::user_transfer_nonce::nonce_object_id(&normalized_from, &client_nonce);
        if self.network_service.state_provider().get_object(&marker_id).is_some() {
            return Self::transfer_err(
                "Duplicate client_nonce: this transfer authorization was already consumed",
            );
        }

        // --- Authorization payload (D5, minimal mandatory set) ---
        let digest = crate::user_transfer_nonce::request_digest(&expected_message);
        let authorization = setu_types::TransferAuthorization::new(
            normalized_from.clone(),
            client_nonce.clone(),
            digest,
        );

        // Forward the canonical subnet id (D1); no local side effects before
        // DAG submission succeeds or fails inside submit_transfer.
        let submit_request = SubmitTransferRequest {
            from: normalized_from,
            to: normalized_to,
            amount: amount_units,
            transfer_type: "setu".to_string(),
            resources: vec![],
            preferred_solver: None,
            shard_id: None,
            subnet_id: Some(subnet_canonical),
            client_nonce: Some(client_nonce),
            chain_id: Some(chain_id),
            authorization: Some(authorization),
        };

        let response = self.network_service.submit_transfer(submit_request).await;

        TransferResponse {
            success: response.success,
            message: response.message,
            event_id: response.event_id,
            estimated_confirmation: Some(2), // ~2 seconds
        }
    }

    // ========== Phase 3: Profile & Subnet Membership ==========

    async fn update_profile(&self, request: UpdateProfileRequest) -> UpdateProfileResponse {
        info!(address = %request.address, "Processing profile update");

        // Validate address format
        if !request.address.starts_with("0x")
            || (request.address.len() != 66 && request.address.len() != 42)
        {
            return UpdateProfileResponse {
                success: false,
                message: "Invalid address format".to_string(),
                event_id: None,
            };
        }

        // Timestamp anti-replay
        if let Err(e) = Self::check_timestamp(request.timestamp) {
            return UpdateProfileResponse { success: false, message: e, event_id: None };
        }

        // Signature verification
        if let Err(e) = Self::verify_signature(
            &request.address, &request.signature, &request.message,
            request.nostr_pubkey.as_deref(), request.public_key.as_deref(),
        ) {
            warn!(address = %request.address, error = %e, "Profile update sig failed");
            return UpdateProfileResponse { success: false, message: e, event_id: None };
        }

        let vlc_snapshot = self.build_vlc_snapshot();
        let attrs = request.attributes.unwrap_or_default();

        let event = match self.network_service.infra_executor().execute_profile_update(
            &request.address,
            request.display_name.as_deref(),
            request.avatar_url.as_deref(),
            request.bio.as_deref(),
            &attrs,
            vlc_snapshot,
        ) {
            Ok(event) => event,
            Err(e) => {
                error!(address = %request.address, error = %e, "Profile update failed");
                return UpdateProfileResponse {
                    success: false, message: format!("Profile update failed: {}", e), event_id: None,
                };
            }
        };

        let event_id = event.id.clone();
        let submit_response = self.network_service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                address = %request.address,
                message = %submit_response.message,
                "Profile update DAG submission failed"
            );
            return UpdateProfileResponse {
                success: false,
                message: submit_response.message,
                event_id: None,
            };
        }

        info!(address = %request.address, event_id = %event_id, "Profile updated");
        UpdateProfileResponse { success: true, message: "Profile updated".to_string(), event_id: Some(event_id) }
    }

    async fn get_profile(&self, address: &str) -> GetProfileResponse {
        let profile_key = format!("profile:{}", address);
        let profile_object_id = ObjectId::new(
            setu_hash_with_domain(b"SETU_PROFILE:", profile_key.as_bytes()),
        );

        match self.network_service.state_provider().get_object(&profile_object_id) {
            Some(data) => {
                let profile: serde_json::Value = serde_json::from_slice(&data).unwrap_or_default();
                GetProfileResponse {
                    found: true,
                    address: address.to_string(),
                    profile: Some(ProfileInfo {
                        display_name: profile["display_name"].as_str().map(|s| s.to_string()),
                        avatar_url: profile["avatar_url"].as_str().map(|s| s.to_string()),
                        bio: profile["bio"].as_str().map(|s| s.to_string()),
                        created_at: profile["created_at"].as_u64().unwrap_or(0),
                    }),
                }
            }
            None => GetProfileResponse {
                found: false,
                address: address.to_string(),
                profile: None,
            },
        }
    }

    async fn join_subnet(&self, request: JoinSubnetRequest) -> JoinSubnetResponse {
        info!(address = %request.address, subnet_id = %request.subnet_id, "Processing subnet join");

        if !request.address.starts_with("0x")
            || (request.address.len() != 66 && request.address.len() != 42)
        {
            return JoinSubnetResponse {
                success: false, message: "Invalid address format".to_string(), event_id: None,
            };
        }

        if let Err(e) = Self::check_timestamp(request.timestamp) {
            return JoinSubnetResponse { success: false, message: e, event_id: None };
        }

        if let Err(e) = Self::verify_signature(
            &request.address, &request.signature, &request.message,
            request.nostr_pubkey.as_deref(), request.public_key.as_deref(),
        ) {
            warn!(address = %request.address, error = %e, "Subnet join sig failed");
            return JoinSubnetResponse { success: false, message: e, event_id: None };
        }

        if self.network_service.get_subnet_info(&request.subnet_id).is_none() {
            return JoinSubnetResponse {
                success: false,
                message: format!("INFRA_SUBNET: Subnet '{}' is not registered", request.subnet_id),
                event_id: None,
            };
        }

        // Duplicate join detection
        let membership_key = format!("user:{}:subnet:{}", request.address, request.subnet_id);
        let membership_oid = ObjectId::new(
            setu_hash_with_domain(b"SETU_MEMBERSHIP:", membership_key.as_bytes()),
        );
        if self.network_service.state_provider().get_object(&membership_oid).is_some() {
            return JoinSubnetResponse {
                success: false,
                message: format!("User {} already a member of subnet '{}'", request.address, request.subnet_id),
                event_id: None,
            };
        }

        let vlc_snapshot = self.build_vlc_snapshot();
        let event = match self.network_service.infra_executor().execute_subnet_join(
            &request.address, &request.subnet_id, vlc_snapshot,
        ) {
            Ok(event) => event,
            Err(e) => {
                error!(address = %request.address, error = %e, "Subnet join failed");
                return JoinSubnetResponse {
                    success: false, message: format!("Subnet join failed: {}", e), event_id: None,
                };
            }
        };

        let event_id = event.id.clone();
        let submit_response = self.network_service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                address = %request.address,
                subnet_id = %request.subnet_id,
                message = %submit_response.message,
                "Subnet join DAG submission failed"
            );
            return JoinSubnetResponse {
                success: false,
                message: submit_response.message,
                event_id: None,
            };
        }

        info!(address = %request.address, subnet_id = %request.subnet_id, event_id = %event_id, "Joined subnet");
        JoinSubnetResponse { success: true, message: "Joined subnet".to_string(), event_id: Some(event_id) }
    }

    async fn leave_subnet(&self, request: LeaveSubnetRequest) -> LeaveSubnetResponse {
        info!(address = %request.address, subnet_id = %request.subnet_id, "Processing subnet leave");

        if !request.address.starts_with("0x")
            || (request.address.len() != 66 && request.address.len() != 42)
        {
            return LeaveSubnetResponse {
                success: false, message: "Invalid address format".to_string(), event_id: None,
            };
        }

        if let Err(e) = Self::check_timestamp(request.timestamp) {
            return LeaveSubnetResponse { success: false, message: e, event_id: None };
        }

        if let Err(e) = Self::verify_signature(
            &request.address, &request.signature, &request.message,
            request.nostr_pubkey.as_deref(), request.public_key.as_deref(),
        ) {
            warn!(address = %request.address, error = %e, "Subnet leave sig failed");
            return LeaveSubnetResponse { success: false, message: e, event_id: None };
        }

        // Existence check: must be a member to leave
        let membership_key = format!("user:{}:subnet:{}", request.address, request.subnet_id);
        let membership_oid = ObjectId::new(
            setu_hash_with_domain(b"SETU_MEMBERSHIP:", membership_key.as_bytes()),
        );
        if self.network_service.state_provider().get_object(&membership_oid).is_none() {
            return LeaveSubnetResponse {
                success: false,
                message: format!("User {} is not a member of subnet '{}'", request.address, request.subnet_id),
                event_id: None,
            };
        }

        let vlc_snapshot = self.build_vlc_snapshot();
        let event = match self.network_service.infra_executor().execute_subnet_leave(
            &request.address, &request.subnet_id, vlc_snapshot,
        ) {
            Ok(event) => event,
            Err(e) => {
                error!(address = %request.address, error = %e, "Subnet leave failed");
                return LeaveSubnetResponse {
                    success: false, message: format!("Subnet leave failed: {}", e), event_id: None,
                };
            }
        };

        let event_id = event.id.clone();
        let submit_response = self.network_service.add_event_to_dag(event).await;
        if !submit_response.success {
            warn!(
                address = %request.address,
                subnet_id = %request.subnet_id,
                message = %submit_response.message,
                "Subnet leave DAG submission failed"
            );
            return LeaveSubnetResponse {
                success: false,
                message: submit_response.message,
                event_id: None,
            };
        }

        info!(address = %request.address, subnet_id = %request.subnet_id, event_id = %event_id, "Left subnet");
        LeaveSubnetResponse { success: true, message: "Left subnet".to_string(), event_id: Some(event_id) }
    }

    async fn check_membership(&self, address: &str, subnet_id: &str) -> CheckMembershipResponse {
        let membership_key = format!("user:{}:subnet:{}", address, subnet_id);
        let membership_oid = ObjectId::new(
            setu_hash_with_domain(b"SETU_MEMBERSHIP:", membership_key.as_bytes()),
        );

        match self.network_service.state_provider().get_object(&membership_oid) {
            Some(data) => {
                let v: serde_json::Value = serde_json::from_slice(&data).unwrap_or_default();
                CheckMembershipResponse {
                    is_member: true,
                    address: address.to_string(),
                    subnet_id: subnet_id.to_string(),
                    joined_at: v["joined_at"].as_u64(),
                }
            }
            None => CheckMembershipResponse {
                is_member: false,
                address: address.to_string(),
                subnet_id: subnet_id.to_string(),
                joined_at: None,
            },
        }
    }

    async fn get_user_subnets(&self, address: &str) -> GetUserSubnetsResponse {
        // Point-query across all registered subnets (O(subnet_count))
        let all_subnets = self.network_service.get_all_subnets();
        let mut joined = Vec::new();

        for subnet_info in &all_subnets {
            let membership_key = format!("user:{}:subnet:{}", address, subnet_info.subnet_id);
            let membership_oid = ObjectId::new(
                setu_hash_with_domain(b"SETU_MEMBERSHIP:", membership_key.as_bytes()),
            );
            if self.network_service.state_provider().get_object(&membership_oid).is_some() {
                joined.push(subnet_info.subnet_id.clone());
            }
        }

        GetUserSubnetsResponse {
            address: address.to_string(),
            subnets: joined,
        }
    }
}

