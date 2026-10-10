-- Actor checkpoints and scheduler checkpoints have independent boundaries.
-- Keep the latest full scheduler state PLUS every patch following it, including
-- patches older than the actor checkpoint. Immutable journal rows stay intact.
CREATE OR REPLACE VIEW marketforge_runtime_mutations AS
SELECT mutation.* FROM marketforge_room_mutations mutation
LEFT JOIN marketforge_recovery_heads head USING (room_id)
WHERE mutation.mutation_seq > COALESCE(head.checkpoint_mutation_seq,0)
   OR (head.snapshot_command_seq IS NULL AND mutation.mutation_seq=COALESCE(head.checkpoint_mutation_seq,0))
UNION
SELECT latest_scheduler.* FROM (
    SELECT DISTINCT ON (room_id) * FROM marketforge_room_mutations
    WHERE mutation_kind='scheduler_progress' ORDER BY room_id, mutation_seq DESC
) latest_scheduler
UNION
SELECT delta.* FROM marketforge_room_mutations delta
WHERE delta.mutation_kind='scheduler_delta'
  AND delta.mutation_seq > COALESCE((
      SELECT MAX(base.mutation_seq) FROM marketforge_room_mutations base
      WHERE base.room_id=delta.room_id AND base.mutation_kind='scheduler_progress'
  ),0)
UNION
SELECT latest_training.* FROM (
    SELECT DISTINCT ON (room_id, payload_json->'run'->'spec'->>'run_id') *
    FROM marketforge_room_mutations WHERE mutation_kind='training_progress'
    ORDER BY room_id, payload_json->'run'->'spec'->>'run_id', mutation_seq DESC
) latest_training;
