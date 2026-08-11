ALTER TABLE executions
    ADD COLUMN requested_capabilities_json TEXT NOT NULL DEFAULT '[]'
    CHECK (
        json_valid(requested_capabilities_json)
        AND json_type(requested_capabilities_json) = 'array'
    );

UPDATE executions
SET requested_capabilities_json = COALESCE(
    (
        SELECT json_group_array(capability.value)
        FROM tasks AS task,
             json_each(task.capabilities_json) AS capability
        WHERE task.id = executions.task_id
          AND capability.value IN (
              'resource_execution',
              'submission_execute',
              'duration_report',
              'discussion',
              'practice'
          )
    ),
    '[]'
);
