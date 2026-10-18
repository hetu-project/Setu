// PR-5 regression: round-aware ConsensusFrame + soft drift outcomes.
//
// These tests live as a crate-level integration test (separate from the
// engine.rs unit tests) so that they exercise only the **public** consensus
// API surface — `ConsensusEngine::new`, `receive_cf`, `advance_round`,
// `current_round`, plus the re-exported `CfReceiveOutcome` and
// `ConsensusFrame`. Anything that compiles against this file is also
// available to downstream crates (setu-validator, setu-solver).
//
// Scope (per docs/feat/post-restart-finality-stall-v3/design.md PR-5):
//   - cf.round is bound into the CF id (verify_id catches tamper)
//   - is_valid_proposer is evaluated against cf.round, not local round
//   - cf.round > local_round → NeedsCatchUp with up_to_depth = depth-1
//   - cf.round < local_round → Stale outcome, never InvalidData
//   - cf.round == local_round on the happy path → Accepted

use consensus::{CfReceiveOutcome, ConsensusEngine, ValidatorSet};
use ed25519_dalek::{Signer, SigningKey};
use rand_core::OsRng;
use setu_types::{Anchor, ConsensusConfig, ConsensusFrame, NodeInfo, VLCSnapshot, ValidatorInfo};

fn make_validator_set() -> ValidatorSet {
    let mut set = ValidatorSet::new();
    for i in 1..=3 {
        let id = format!("v{}", i);
        let mut node = NodeInfo::new_validator(
            id.clone(),
            "127.0.0.1".to_string(),
            9000 + i as u16,
        );
        let signing_key = SigningKey::generate(&mut OsRng);
        node.public_key = signing_key.verifying_key().to_bytes().to_vec();
        let signature = signing_key.sign(id.as_bytes()).to_bytes().to_vec();
        set.add_validator(ValidatorInfo::new(node, false).with_signature(signature));
    }
    set
}

fn make_config() -> ConsensusConfig {
    ConsensusConfig {
        vlc_delta_threshold: 1,
        min_events_per_cf: 1,
        max_events_per_cf: 1000,
        cf_timeout_ms: 5000,
        validator_count: 3,
    }
}

fn make_anchor(depth: u64) -> Anchor {
    Anchor::new(
        vec![],
        VLCSnapshot::default(),
        format!("state-root-{}", depth),
        None,
        depth,
    )
}

/// Drive `engine.current_round()` to the requested value by advancing the
/// validator-set round counter. Used to set up the "local is ahead" / "local
/// is behind" scenarios without standing up a real consensus run.
async fn advance_to(engine: &ConsensusEngine, target: u64) {
    while engine.current_round().await < target {
        engine.advance_round().await;
    }
}

/// Look up which validator the local set considers the leader for `round`.
/// This sidesteps having to hard-code the rotation policy in the test.
fn proposer_for_round(round: u64) -> String {
    let set = make_validator_set();
    set.get_valid_proposer(round)
        .expect("validator set must have a proposer for round 0..=2")
}

#[tokio::test]
async fn cf_round_is_bound_into_id() {
    // Two CFs that differ only in `round` must have different ids — that is
    // the entire reason the V2 domain exists. Without this property the
    // forge-tamper test below has no teeth.
    let anchor = make_anchor(0);
    let proposer = proposer_for_round(0);
    let cf0 = ConsensusFrame::new(0, anchor.clone(), proposer.clone());
    let cf1 = ConsensusFrame::new(1, anchor, proposer);
    assert_ne!(
        cf0.id, cf1.id,
        "CFs with different round but identical (anchor, proposer) must have distinct ids"
    );
}

#[tokio::test]
async fn cf_forged_round_is_rejected_by_verify_id() {
    // Mutate `round` post-construction; verify_id should refuse the CF when
    // receive_cf re-derives the id from the current fields.
    let anchor = make_anchor(0);
    let proposer = proposer_for_round(0);
    let mut cf = ConsensusFrame::new(0, anchor, proposer);
    cf.round = 7; // forge — id was computed with round=0

    let engine = ConsensusEngine::new(make_config(), "v2".to_string(), make_validator_set());

    let err = engine
        .receive_cf(cf)
        .await
        .expect_err("forged round must fail verify_id");
    let msg = err.to_string();
    assert!(
        msg.contains("Invalid") || msg.contains("invalid") || msg.contains("id"),
        "rejection should mention id/Invalid, got: {}",
        msg
    );
}

#[tokio::test]
async fn cf_round_drives_proposer_check_not_local_round() {
    // Local engine sits at round 0. We submit a CF that claims round 2 and
    // names the validator who is the *legitimate* proposer for round 2.
    // Pre-PR-4 this was rejected because the engine compared against
    // `vs.current_round()` (= 0). Post-PR-4 the proposer check is keyed by
    // `cf.round`, so the early `InvalidLeader` rejection no longer fires and
    // the outcome instead routes through the drift policy → NeedsCatchUp.
    let engine = ConsensusEngine::new(make_config(), "v2".to_string(), make_validator_set());
    assert_eq!(engine.current_round().await, 0);

    let proposer_r2 = proposer_for_round(2);
    let cf = ConsensusFrame::new(2, make_anchor(3), proposer_r2);

    let outcome = engine
        .receive_cf(cf)
        .await
        .expect("legitimate round-2 proposer must not be rejected by proposer check");

    match outcome {
        CfReceiveOutcome::NeedsCatchUp { up_to_depth, cf } => {
            assert_eq!(
                up_to_depth, 2,
                "up_to_depth should be cf.anchor.depth - 1 (= 3 - 1)"
            );
            assert_eq!(cf.round, 2);
        }
        other => panic!(
            "expected NeedsCatchUp for cf.round > local_round (got {:?})",
            other
        ),
    }
}

#[tokio::test]
async fn cf_with_round_ahead_returns_needs_catch_up() {
    let engine = ConsensusEngine::new(make_config(), "v3".to_string(), make_validator_set());

    let proposer = proposer_for_round(5);
    let cf = ConsensusFrame::new(5, make_anchor(10), proposer);

    match engine
        .receive_cf(cf)
        .await
        .expect("ahead-round CF must not error")
    {
        CfReceiveOutcome::NeedsCatchUp { up_to_depth, cf } => {
            assert_eq!(up_to_depth, 9);
            assert_eq!(cf.round, 5);
            assert_eq!(cf.anchor.depth, 10);
        }
        other => panic!("expected NeedsCatchUp, got {:?}", other),
    }
}

#[tokio::test]
async fn cf_with_round_behind_returns_stale_not_error() {
    // Move local engine to round 5, then feed in a CF for round 2.
    let engine = ConsensusEngine::new(make_config(), "v1".to_string(), make_validator_set());
    advance_to(&engine, 5).await;
    assert_eq!(engine.current_round().await, 5);

    let proposer = proposer_for_round(2);
    let cf = ConsensusFrame::new(2, make_anchor(0), proposer);

    match engine
        .receive_cf(cf)
        .await
        .expect("behind-round CF must be Stale, never InvalidData")
    {
        CfReceiveOutcome::Stale {
            cf_round,
            local_round,
        } => {
            assert_eq!(cf_round, 2);
            assert_eq!(local_round, 5);
        }
        other => panic!("expected Stale, got {:?}", other),
    }
}

#[tokio::test]
async fn cf_with_matching_round_is_accepted() {
    // Sanity check that the happy-path branch is reached untouched — the
    // drift policy is *additive*, never a regression on the in-round case.
    // We don't drive finalization here (no votes attached); we only assert
    // that the outcome variant is `Accepted` with `finalized = false`.
    let engine = ConsensusEngine::new(make_config(), "v2".to_string(), make_validator_set());

    let proposer = proposer_for_round(0);
    let cf = ConsensusFrame::new(0, make_anchor(0), proposer);

    match engine
        .receive_cf(cf)
        .await
        .expect("in-round CF must be accepted")
    {
        CfReceiveOutcome::Accepted { finalized, .. } => {
            assert!(
                !finalized,
                "single-vote CF should not be finalized yet (no quorum)"
            );
        }
        other => panic!("expected Accepted, got {:?}", other),
    }
}
