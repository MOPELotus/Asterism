DROP TRIGGER IF EXISTS trg_chaoxing_execution_verification_log;

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
           CASE
             WHEN lower(NEW.message) LIKE '%source=scan%' THEN 'scan'
             WHEN lower(NEW.message) LIKE '%source=question_read%' THEN 'question_read'
             WHEN lower(NEW.message) LIKE '%source=assessment%' THEN 'assessment'
             ELSE 'execution'
           END,
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
