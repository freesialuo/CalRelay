//! Read-only runtime scheduling hint; reconcile still owns every job/watermark write.
use crate::{planning::db_err, Storage};
use calendar_application::{PlanningError, PlanningRequest};
impl Storage {
    /// A matching persisted window survives restart. Revisions, tzdata, extension threshold
    /// or a backwards UTC jump beyond the current horizon require reconciliation again.
    /// This hint is not a fence: reconcile rechecks all generations in its transaction.
    pub async fn planning_required(
        &self,
        request: &PlanningRequest,
        now: i64,
    ) -> Result<bool, PlanningError> {
        request.validate()?;
        let row: Option<(i64, i64, i64, i64, String)> = sqlx::query_as(
            "SELECT source_revision,rule_revision,output_revision,planned_until,tzdata_version FROM planning_watermarks WHERE source_id=? AND rule_id=? AND output_id=?")
            .bind(&request.source_id).bind(&request.rule_id).bind(&request.output_id)
            .fetch_optional(self.pool()).await.map_err(db_err)?;
        let target = now
            .checked_add(request.horizon_ms)
            .ok_or(PlanningError::InvalidWindow)?;
        Ok(match row {
            None => true,
            Some((source, rule, output, until, tzdata)) => {
                source != request.base_revision
                    || rule != request.rule_generation
                    || output != request.output_generation
                    || tzdata != request.tzdata_version
                    || until <= target.saturating_sub(request.extension_interval_ms)
                    || until > target
            }
        })
    }
}
