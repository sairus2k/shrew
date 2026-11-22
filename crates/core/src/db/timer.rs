//! The timer that is running now: at most one, kept in SQLite so that it
//! survives a restart and a crash.

use super::worklogs::{self, NewWorklog};
use crate::models::{IssueKey, RunningTimer};

pub(crate) async fn get(
    executor: impl sqlx::SqliteExecutor<'_>,
) -> sqlx::Result<Option<RunningTimer>> {
    let row: Option<(IssueKey, i64)> =
        sqlx::query_as("SELECT issue_key, started_at FROM running_timer")
            .fetch_optional(executor)
            .await?;
    Ok(row.map(|(issue_key, started_at)| RunningTimer {
        issue_key,
        started_at,
    }))
}

/// A timer that is already running is replaced and its time is not logged.
pub(crate) async fn start(
    executor: impl sqlx::SqliteExecutor<'_>,
    timer: &RunningTimer,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO running_timer (id, issue_key, started_at) VALUES (1, ?, ?)",
    )
    .bind(&timer.issue_key)
    .bind(timer.started_at)
    .execute(executor)
    .await?;
    Ok(())
}

/// Turns the timer into a worklog for the time up to `stopped_at` and deletes
/// it, in one transaction: a crash leaves the timer running or the worklog
/// logged, never both and never neither. Returns the issue that got the
/// worklog. A timer that ran for less than the shortest worklog Jira takes,
/// as an accidental start does or one whose clock was set back, is deleted
/// and logs nothing.
pub(crate) async fn stop(
    pool: &sqlx::SqlitePool,
    stopped_at: i64,
) -> anyhow::Result<Option<IssueKey>> {
    let mut tx = crate::db::begin(pool).await?;
    let row: Option<(IssueKey, i64)> =
        sqlx::query_as("DELETE FROM running_timer RETURNING issue_key, started_at")
            .fetch_optional(&mut *tx)
            .await?;
    let Some((issue_key, started_at)) = row else {
        return Ok(None);
    };
    let timer = RunningTimer {
        issue_key,
        started_at,
    };
    let time_spent_sec = i64::try_from(timer.elapsed_secs(stopped_at))?;
    if time_spent_sec < worklogs::MIN_TIME_SPENT_SEC {
        tx.commit().await?;
        return Ok(None);
    }
    let Some(started_at) = worklogs::started_at_from_unix(started_at) else {
        // The row cannot become a worklog; the timer stays as it was
        return Err(anyhow::anyhow!(
            "the start time of the timer is out of range"
        ));
    };
    worklogs::log(
        &mut tx,
        &NewWorklog {
            issue_key: timer.issue_key.clone(),
            time_spent_sec,
            started_at,
            comment: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Some(timer.issue_key))
}

/// Follows an issue that was moved to another project.
pub(crate) async fn rename_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    old: &IssueKey,
    new: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE running_timer SET issue_key = ? WHERE issue_key = ?")
        .bind(new)
        .bind(old)
        .execute(executor)
        .await?;
    Ok(())
}

pub(crate) async fn delete_for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM running_timer WHERE issue_key = ?")
        .bind(key)
        .execute(executor)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    fn timer(key: &str, started_at: i64) -> RunningTimer {
        RunningTimer {
            issue_key: key.into(),
            started_at,
        }
    }

    async fn worklogs(pool: &SqlitePool) -> Vec<(String, i64, String)> {
        sqlx::query_as("SELECT issue_key, time_spent_sec, started_at FROM worklogs")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn queue(pool: &SqlitePool) -> Vec<(String, String, String)> {
        sqlx::query_as("SELECT entity_type, issue_key, payload FROM sync_queue")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn stop_turns_the_running_timer_into_a_worklog_and_an_outbox_entry(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_774_771_200)).await.unwrap();
        // This row is what a restarted app reads back
        assert_eq!(
            get(&pool).await.unwrap(),
            Some(timer("WEB-1", 1_774_771_200))
        );

        let stopped = stop(&pool, 1_774_771_290).await.unwrap();

        assert_eq!(stopped, Some("WEB-1".into()));
        assert_eq!(get(&pool).await.unwrap(), None);
        assert_eq!(
            worklogs(&pool).await,
            [(
                "WEB-1".to_owned(),
                90,
                "2026-03-29T08:00:00.000Z".to_owned()
            )]
        );
        let queued = queue(&pool).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(
            (queued[0].0.as_str(), queued[0].1.as_str()),
            ("worklog", "WEB-1")
        );
        let payload: serde_json::Value = serde_json::from_str(&queued[0].2).unwrap();
        assert_eq!(payload["time_spent_seconds"], 90);
        assert_eq!(payload["started_at"], "2026-03-29T08:00:00.000Z");
        let local_id: String = sqlx::query_scalar("SELECT local_id FROM worklogs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(payload["local_id"], local_id);
    }

    #[sqlx::test]
    async fn stop_is_one_transaction(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_774_771_200)).await.unwrap();
        // The outbox refuses the entry, so the worklog and the delete must roll back
        sqlx::raw_sql(
            "CREATE TRIGGER refuse BEFORE INSERT ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(stop(&pool, 1_774_771_290).await.is_err());

        assert_eq!(
            get(&pool).await.unwrap(),
            Some(timer("WEB-1", 1_774_771_200))
        );
        assert!(worklogs(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn stopping_with_no_timer_logs_nothing(pool: SqlitePool) {
        assert_eq!(stop(&pool, 1_774_771_290).await.unwrap(), None);
        assert!(worklogs(&pool).await.is_empty());
        assert!(queue(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn a_timer_stopped_within_its_first_minute_logs_nothing(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_774_771_200)).await.unwrap();

        assert_eq!(stop(&pool, 1_774_771_259).await.unwrap(), None);

        assert_eq!(get(&pool).await.unwrap(), None);
        assert!(worklogs(&pool).await.is_empty());
        assert!(queue(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn a_timer_stopped_after_a_minute_logs_it(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_774_771_200)).await.unwrap();

        assert_eq!(
            stop(&pool, 1_774_771_260).await.unwrap(),
            Some("WEB-1".into())
        );

        assert_eq!(worklogs(&pool).await[0].1, 60);
    }

    #[sqlx::test]
    async fn a_clock_set_back_logs_no_negative_time(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_774_771_200)).await.unwrap();

        assert_eq!(stop(&pool, 1_774_771_000).await.unwrap(), None);

        assert_eq!(get(&pool).await.unwrap(), None);
        assert!(worklogs(&pool).await.is_empty());
        assert!(queue(&pool).await.is_empty());
    }

    #[test]
    fn the_elapsed_time_of_a_clock_set_back_is_zero() {
        let timer = timer("WEB-1", 1_000);
        assert_eq!(timer.elapsed_secs(1_090), 90);
        assert_eq!(timer.elapsed_secs(400), 0);
    }

    #[sqlx::test]
    async fn starting_a_second_timer_replaces_the_first_without_logging_it(pool: SqlitePool) {
        start(&pool, &timer("WEB-1", 1_000)).await.unwrap();
        start(&pool, &timer("WEB-2", 2_000)).await.unwrap();

        assert_eq!(get(&pool).await.unwrap(), Some(timer("WEB-2", 2_000)));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM running_timer")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert!(worklogs(&pool).await.is_empty());
        assert!(queue(&pool).await.is_empty());
    }
}
