CREATE TABLE chaoxing_verification_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    provider_account_id TEXT NOT NULL,
    execution_id TEXT,
    occurred_at TEXT NOT NULL,
    verification_type TEXT NOT NULL CHECK (verification_type IN (
        'image_captcha', 'exam_slider', 'face', 'policy', 'unknown'
    )),
    state TEXT NOT NULL CHECK (state IN (
        'configured', 'started', 'succeeded', 'failed', 'budget_exhausted'
    )),
    source TEXT NOT NULL CHECK (source IN ('execution', 'scan', 'question_read', 'assessment')),
    next_retry_at TEXT,
    detail_sanitized TEXT NOT NULL,
    UNIQUE(execution_id, occurred_at, verification_type, state),
    FOREIGN KEY(provider_account_id) REFERENCES provider_accounts(id) ON DELETE CASCADE,
    FOREIGN KEY(execution_id) REFERENCES executions(id) ON DELETE CASCADE
);

CREATE INDEX idx_chaoxing_verification_account_time
    ON chaoxing_verification_attempts(provider_account_id, occurred_at DESC);

CREATE TRIGGER trg_chaoxing_execution_verification_log
AFTER INSERT ON execution_logs
WHEN lower(NEW.message) LIKE 'verification %'
BEGIN
    INSERT OR IGNORE INTO chaoxing_verification_attempts (
        id, provider_account_id, execution_id, occurred_at, verification_type,
        state, source, next_retry_at, detail_sanitized
    )
    SELECT lower(hex(randomblob(16))), task.provider_account_id, NEW.execution_id,
           NEW.timestamp,
           CASE
             WHEN lower(NEW.message) LIKE '%image_captcha%' THEN 'image_captcha'
             WHEN lower(NEW.message) LIKE '%slider%' THEN 'exam_slider'
             WHEN lower(NEW.message) LIKE '%face%' THEN 'face'
             WHEN lower(NEW.message) LIKE '%policy%' THEN 'policy'
             ELSE 'unknown'
           END,
           CASE
             WHEN lower(NEW.message) LIKE '%budget%exhausted%' THEN 'budget_exhausted'
             WHEN lower(NEW.message) LIKE '%succeeded%' THEN 'succeeded'
             WHEN lower(NEW.message) LIKE '%failed%' THEN 'failed'
             WHEN lower(NEW.message) LIKE '%started%' OR lower(NEW.message) LIKE '%attempt%' THEN 'started'
             ELSE 'configured'
           END,
           'execution',
           CASE
             WHEN lower(NEW.message) LIKE '%failed%' OR lower(NEW.message) LIKE '%budget%exhausted%'
             THEN datetime(NEW.timestamp, '+5 minutes')
             ELSE NULL
           END,
           CASE
             WHEN lower(NEW.message) LIKE '%image_captcha%' AND lower(NEW.message) LIKE '%succeeded%' THEN 'image captcha succeeded'
             WHEN lower(NEW.message) LIKE '%image_captcha%' AND lower(NEW.message) LIKE '%failed%' THEN 'image captcha failed'
             WHEN lower(NEW.message) LIKE '%image_captcha%' THEN 'image captcha started'
             WHEN lower(NEW.message) LIKE '%slider%' AND lower(NEW.message) LIKE '%succeeded%' THEN 'exam slider succeeded'
             WHEN lower(NEW.message) LIKE '%slider%' AND lower(NEW.message) LIKE '%failed%' THEN 'exam slider failed'
             WHEN lower(NEW.message) LIKE '%slider%' THEN 'exam slider started'
             WHEN lower(NEW.message) LIKE '%face%' AND lower(NEW.message) LIKE '%succeeded%' THEN 'face verification succeeded'
             WHEN lower(NEW.message) LIKE '%face%' AND lower(NEW.message) LIKE '%failed%' THEN 'face verification failed'
             WHEN lower(NEW.message) LIKE '%face%' THEN 'face verification started'
             WHEN lower(NEW.message) LIKE '%budget%exhausted%' THEN 'verification budget exhausted'
             ELSE 'verification policy configured'
           END
    FROM executions AS execution
    INNER JOIN tasks AS task ON task.id = execution.task_id
    INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id
    WHERE execution.id = NEW.execution_id AND account.provider_id = 'chaoxing';
END;

INSERT OR IGNORE INTO chaoxing_verification_attempts (
    id, provider_account_id, execution_id, occurred_at, verification_type,
    state, source, next_retry_at, detail_sanitized
)
SELECT lower(hex(randomblob(16))), task.provider_account_id, logs.execution_id,
       logs.timestamp,
       CASE
         WHEN lower(logs.message) LIKE '%image_captcha%' THEN 'image_captcha'
         WHEN lower(logs.message) LIKE '%slider%' THEN 'exam_slider'
         WHEN lower(logs.message) LIKE '%face%' THEN 'face'
         WHEN lower(logs.message) LIKE '%policy%' THEN 'policy'
         ELSE 'unknown'
       END,
       CASE
         WHEN lower(logs.message) LIKE '%budget%exhausted%' THEN 'budget_exhausted'
         WHEN lower(logs.message) LIKE '%succeeded%' THEN 'succeeded'
         WHEN lower(logs.message) LIKE '%failed%' THEN 'failed'
         WHEN lower(logs.message) LIKE '%started%' OR lower(logs.message) LIKE '%attempt%' THEN 'started'
         ELSE 'configured'
       END,
       'execution',
       CASE
         WHEN lower(logs.message) LIKE '%failed%' OR lower(logs.message) LIKE '%budget%exhausted%'
         THEN datetime(logs.timestamp, '+5 minutes')
         ELSE NULL
       END,
       'historical sanitized verification observation'
FROM execution_logs AS logs
INNER JOIN executions AS execution ON execution.id = logs.execution_id
INNER JOIN tasks AS task ON task.id = execution.task_id
INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id
WHERE account.provider_id = 'chaoxing' AND lower(logs.message) LIKE 'verification %';
