#![cfg(test)]

extern crate std;

use soroban_sdk::{testutils::Address as _, Address, Env, String};
use tipjar::{
    retroactive_funding::{EvalCriteria, RoundStatus},
    TipJarContract, TipJarContractClient,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, TipJarContractClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, TipJarContract);
    let client = TipJarContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract(token_admin.clone());

    client.init(&admin);
    client.add_token(&admin, &token);

    (env, client, admin, token)
}

fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    soroban_sdk::token::StellarAssetClient::new(env, token).mint(to, &amount);
}

fn advance(env: &Env, secs: u64) {
    env.ledger().with_mut(|l| l.timestamp += secs);
}

fn criteria(env: &Env) -> EvalCriteria {
    EvalCriteria {
        min_tip_volume: 100,
        min_unique_tippers: 2,
        description: String::from_str(env, "Impact on public goods"),
    }
}

fn make_round(
    env: &Env,
    client: &TipJarContractClient,
    admin: &Address,
    token: &Address,
    pool: i128,
) -> u64 {
    mint(env, token, admin, pool);
    let now = env.ledger().timestamp();
    client.rf_create_round(admin, token, &pool, &criteria(env), &(now + 10), &(now + 3610))
}

fn nominate(
    env: &Env,
    client: &TipJarContractClient,
    round_id: u64,
    project: &Address,
) {
    client.rf_nominate_project(
        &round_id,
        project,
        &500,
        &3,
        &String::from_str(env, "Built open-source tooling"),
    );
}

// ── create_round ──────────────────────────────────────────────────────────────

#[test]
fn test_create_round() {
    let (env, client, admin, token) = setup();
    let id = make_round(&env, &client, &admin, &token, 10_000);
    assert_eq!(id, 0);
    let round = client.rf_get_round(&id).unwrap();
    assert_eq!(round.reward_pool, 10_000);
    assert_eq!(round.status, RoundStatus::Nominations);
}

#[test]
fn test_create_multiple_rounds_increments_id() {
    let (env, client, admin, token) = setup();
    mint(&env, &token, &admin, 3_000);
    let now = env.ledger().timestamp();
    let r0 = client.rf_create_round(&admin, &token, &1_000, &criteria(&env), &(now + 10), &(now + 3610));
    let r1 = client.rf_create_round(&admin, &token, &1_000, &criteria(&env), &(now + 10), &(now + 3610));
    let r2 = client.rf_create_round(&admin, &token, &1_000, &criteria(&env), &(now + 10), &(now + 3610));
    assert_eq!((r0, r1, r2), (0, 1, 2));
}

// ── nominate_project ──────────────────────────────────────────────────────────

#[test]
fn test_nominate_project() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);

    let rec = client.rf_get_project(&round_id, &project).unwrap();
    assert_eq!(rec.tip_volume, 500);
    assert_eq!(rec.unique_tippers, 3);
    assert_eq!(rec.votes, 0);
}

#[test]
fn test_nominate_below_criteria_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);

    // tip_volume below min (100)
    let result = client.try_rf_nominate_project(
        &round_id,
        &project,
        &50,
        &3,
        &String::from_str(&env, "low volume"),
    );
    assert!(result.is_err());
}

#[test]
fn test_nominate_duplicate_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);

    let result = client.try_rf_nominate_project(
        &round_id,
        &project,
        &500,
        &3,
        &String::from_str(&env, "again"),
    );
    assert!(result.is_err());
}

#[test]
fn test_get_round_projects() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let p1 = Address::generate(&env);
    let p2 = Address::generate(&env);
    nominate(&env, &client, round_id, &p1);
    nominate(&env, &client, round_id, &p2);

    let projects = client.rf_get_round_projects(&round_id);
    assert_eq!(projects.len(), 2);
}

// ── open_voting ───────────────────────────────────────────────────────────────

#[test]
fn test_open_voting() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    client.rf_open_voting(&admin, &round_id);
    let round = client.rf_get_round(&round_id).unwrap();
    assert_eq!(round.status, RoundStatus::Voting);
}

#[test]
fn test_open_voting_unauthorized_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let other = Address::generate(&env);
    assert!(client.try_rf_open_voting(&other, &round_id).is_err());
}

// ── cast_vote ─────────────────────────────────────────────────────────────────

#[test]
fn test_cast_vote() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);
    client.rf_open_voting(&admin, &round_id);

    // Advance into voting window.
    advance(&env, 11);

    let voter = Address::generate(&env);
    client.rf_cast_vote(&voter, &round_id, &project, &100);

    assert!(client.rf_has_voted(&round_id, &voter));
    let rec = client.rf_get_project(&round_id, &project).unwrap();
    assert_eq!(rec.votes, 100);
}

#[test]
fn test_double_vote_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);
    client.rf_open_voting(&admin, &round_id);
    advance(&env, 11);

    let voter = Address::generate(&env);
    client.rf_cast_vote(&voter, &round_id, &project, &100);
    assert!(client.try_rf_cast_vote(&voter, &round_id, &project, &50).is_err());
}

#[test]
fn test_vote_outside_window_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);
    client.rf_open_voting(&admin, &round_id);

    // Do NOT advance — before voting_start.
    let voter = Address::generate(&env);
    assert!(client.try_rf_cast_vote(&voter, &round_id, &project, &100).is_err());
}

// ── finalize_and_distribute ───────────────────────────────────────────────────

#[test]
fn test_finalize_distributes_proportionally() {
    let (env, client, admin, token) = setup();
    let pool: i128 = 10_000;
    let round_id = make_round(&env, &client, &admin, &token, pool);

    let p1 = Address::generate(&env);
    let p2 = Address::generate(&env);
    nominate(&env, &client, round_id, &p1);
    nominate(&env, &client, round_id, &p2);
    client.rf_open_voting(&admin, &round_id);
    advance(&env, 11);

    // p1 gets 3× the votes of p2 → should get 3× the reward.
    let v1 = Address::generate(&env);
    let v2 = Address::generate(&env);
    let v3 = Address::generate(&env);
    let v4 = Address::generate(&env);
    client.rf_cast_vote(&v1, &round_id, &p1, &300);
    client.rf_cast_vote(&v2, &round_id, &p1, &300);
    client.rf_cast_vote(&v3, &round_id, &p1, &300);
    client.rf_cast_vote(&v4, &round_id, &p2, &300);

    // Advance past voting_end.
    advance(&env, 3600);
    client.rf_finalize_and_distribute(&admin, &round_id);

    let round = client.rf_get_round(&round_id).unwrap();
    assert_eq!(round.status, RoundStatus::Finalized);

    let token_client = soroban_sdk::token::TokenClient::new(&env, &token);
    let bal1 = token_client.balance(&p1);
    let bal2 = token_client.balance(&p2);
    assert_eq!(bal1 + bal2, pool);
    assert!(bal1 > bal2, "p1 (3× votes) should receive more: {bal1} vs {bal2}");
}

#[test]
fn test_finalize_before_voting_ends_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);
    client.rf_open_voting(&admin, &round_id);
    advance(&env, 11); // inside window

    assert!(client.try_rf_finalize_and_distribute(&admin, &round_id).is_err());
}

#[test]
fn test_finalize_twice_fails() {
    let (env, client, admin, token) = setup();
    let round_id = make_round(&env, &client, &admin, &token, 5_000);
    let project = Address::generate(&env);
    nominate(&env, &client, round_id, &project);
    client.rf_open_voting(&admin, &round_id);
    advance(&env, 3620);
    client.rf_finalize_and_distribute(&admin, &round_id);

    assert!(client.try_rf_finalize_and_distribute(&admin, &round_id).is_err());
}

#[test]
fn test_equal_split_when_no_votes() {
    let (env, client, admin, token) = setup();
    let pool: i128 = 1_000;
    let round_id = make_round(&env, &client, &admin, &token, pool);

    let p1 = Address::generate(&env);
    let p2 = Address::generate(&env);
    nominate(&env, &client, round_id, &p1);
    nominate(&env, &client, round_id, &p2);
    client.rf_open_voting(&admin, &round_id);
    advance(&env, 3620); // skip past voting window without voting

    client.rf_finalize_and_distribute(&admin, &round_id);

    let token_client = soroban_sdk::token::TokenClient::new(&env, &token);
    // Both projects should receive equal shares.
    assert_eq!(token_client.balance(&p1), 500);
    assert_eq!(token_client.balance(&p2), 500);
}
