CREATE TEMP TABLE answer_bootstrap_harvest_backfill (
    owner_user_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL PRIMARY KEY,
    created_at TEXT NOT NULL,
    harvest_id TEXT NOT NULL UNIQUE,
    schedule_id TEXT NOT NULL UNIQUE
) STRICT;

INSERT INTO answer_bootstrap_harvest_backfill (
    owner_user_id,
    provider_id,
    provider_account_id,
    created_at,
    harvest_id,
    schedule_id
)
SELECT
    account.owner_user_id,
    account.provider_id,
    account.id,
    account.updated_at,
    lower(hex(randomblob(16))),
    lower(hex(randomblob(16)))
FROM provider_accounts AS account
WHERE account.auth_state_json = '"authenticated"'
  AND NOT EXISTS (
      SELECT 1
      FROM answer_bootstrap_harvests AS harvest
      WHERE harvest.provider_account_id = account.id
        AND harvest.generation = 1
  );

INSERT INTO scheduled_jobs (
    id,
    job_kind,
    payload_json,
    run_at,
    state,
    attempts,
    idempotency_key,
    created_at,
    updated_at
)
SELECT
    schedule_id,
    'answer_bootstrap_harvest',
    '{"kind":"answer_bootstrap_harvest","payload":{"harvest_id":"'
        || harvest_id
        || '","provider_account_id":"'
        || provider_account_id
        || '","generation":1}}',
    created_at,
    'pending',
    0,
    'answer-bootstrap-harvest:' || provider_account_id || ':1',
    created_at,
    created_at
FROM answer_bootstrap_harvest_backfill;

INSERT INTO answer_bootstrap_harvests (
    id,
    owner_user_id,
    provider_id,
    provider_account_id,
    generation,
    schedule_id,
    state,
    scanned_task_count,
    total_task_count,
    watermark_sanitized_json,
    created_at,
    started_at,
    updated_at,
    completed_at
)
SELECT
    harvest_id,
    owner_user_id,
    provider_id,
    provider_account_id,
    1,
    schedule_id,
    'pending',
    0,
    NULL,
    '{}',
    created_at,
    NULL,
    created_at,
    NULL
FROM answer_bootstrap_harvest_backfill;

DROP TABLE answer_bootstrap_harvest_backfill;
